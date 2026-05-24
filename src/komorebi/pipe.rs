//! Async worker that subscribes to komorebi via a Windows named pipe and
//! emits debounced [`WorldState`] snapshots over an mpsc channel.
//!
//! See the technical-design section of `plan.md` for the rationale; in short:
//! we don't try to model every komorebi event variant — every event triggers
//! a `komorebic state` re-query (coalesced with a ~50 ms debounce), which is
//! the canonical source of truth and is robust to komorebi schema evolution.

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep, Instant};

use crate::komorebi::{state::WorldState, types};

/// Coalesce bursts of komorebi events into a single `komorebic state` query
/// at most every `DEBOUNCE` window.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// Initial delay between reconnect attempts when the pipe breaks.
const BACKOFF_MIN: Duration = Duration::from_millis(500);

/// Upper bound on the reconnect backoff delay.
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// A connection that stayed up at least this long is considered "healthy" —
/// when it terminates, we reset the reconnect backoff to its minimum.
const HEALTHY_SESSION: Duration = Duration::from_secs(5);

/// Windows named-pipe path prefix (the part komorebic does *not* include in
/// `subscribe-pipe <name>`).
const PIPE_PREFIX: &str = r"\\.\pipe\";

/// Win32 `CREATE_NO_WINDOW` flag — suppresses the console window when this
/// (windowed-subsystem) process spawns a console-subsystem subprocess such
/// as `komorebic.exe`. Without this flag, every spawn would briefly flash
/// (and pile up, if it loops) a black `cmd.exe`-style window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a `tokio::process::Command` for `komorebic` that never shows a
/// console window. All callers in this module **must** use this helper.
fn komorebic(args: &[&str]) -> Command {
    // tokio::process::Command exposes `creation_flags` natively on Windows;
    // no need to import `std::os::windows::process::CommandExt`.
    let mut cmd = Command::new("komorebic");
    cmd.args(args);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(std::process::Stdio::null());
    cmd
}

/// Build a unique pipe name for this process (komorebi appends its own path
/// prefix, so we only need a process-unique suffix).
pub fn unique_pipe_name() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let nonce = nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(pid);
    format!("komorebi-tray-grid-{pid}-{nonce:016x}")
}

/// Run the worker until the channel is closed by the consumer.
///
/// The worker:
/// 1. Sends an initial [`WorldState`] derived from `komorebic state`.
/// 2. Creates a uniquely named Windows named pipe and registers it with
///    `komorebic subscribe-pipe`.
/// 3. Reads NDJSON events from the pipe. After every event (debounced) it
///    re-runs `komorebic state` and sends a fresh [`WorldState`].
/// 4. On EOF or any I/O error, reconnects with exponential backoff. The
///    backoff is reset only when the previous session stayed alive for at
///    least [`HEALTHY_SESSION`], so a flapping komorebi can never cause a
///    spawn storm.
pub async fn run_worker(tx: UnboundedSender<WorldState>) -> Result<()> {
    // Seed the UI immediately so the user sees something even if komorebi is
    // briefly unreachable. Failures here just get logged; the subscribe loop
    // will try again later.
    push_fresh_state(&tx).await;

    let mut backoff = BACKOFF_MIN;
    loop {
        if tx.is_closed() {
            return Ok(());
        }

        let started = Instant::now();
        let outcome = subscribe_loop(&tx).await;
        let session = started.elapsed();

        match outcome {
            Ok(()) => tracing::info!(
                session_ms = session.as_millis() as u64,
                "komorebi pipe peer disconnected; reconnecting",
            ),
            Err(e) => tracing::warn!(
                error = ?e,
                session_ms = session.as_millis() as u64,
                backoff_ms = backoff.as_millis() as u64,
                "komorebi pipe worker error; backing off",
            ),
        }

        if session >= HEALTHY_SESSION {
            backoff = BACKOFF_MIN;
        }

        sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);

        // Re-sync via `komorebic state` *after* the backoff: even if the
        // event pipe is broken, the UI should reflect the latest snapshot.
        if tx.is_closed() {
            return Ok(());
        }
        push_fresh_state(&tx).await;
    }
}

async fn push_fresh_state(tx: &UnboundedSender<WorldState>) {
    match fetch_state().await {
        Ok(state) => {
            let _ = tx.send(state);
        }
        Err(e) => tracing::warn!(error = ?e, "`komorebic state` failed"),
    }
}

/// How long we tolerate `komorebic` taking to finish a one-shot subprocess
/// call (`subscribe-pipe`, `state`) before we treat the call as hung and
/// recycle. Generous — `komorebic` is normally near-instantaneous.
const KOMOREBIC_TIMEOUT: Duration = Duration::from_secs(5);

/// While waiting for komorebi to connect to a freshly-subscribed pipe, we
/// poll `komorebic state` every `HEALTH_INTERVAL` to (a) detect that komorebi
/// went away between subscription and its first event and (b) push a fresh
/// snapshot so the UI doesn't go stale.
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);

/// Hard ceiling on how long we wait for komorebi to open its end of the
/// pipe after we subscribe. If no event arrives in this window, we recycle
/// the subscription. This guards against the narrow race where
/// `subscribe-pipe` registers us on a komorebi process that's about to die:
/// the next `komorebic state` would succeed (it would talk to the *new*
/// komorebi) yet our pipe would never receive a client. Re-subscribing on a
/// fresh pipe is the only reliable recovery.
const CONNECT_MAX_WAIT: Duration = Duration::from_secs(60);

/// Inner loop: connect to komorebi once, read events until EOF or error, then
/// return.
///
/// Reconnection contract — every iteration of the outer `run_worker` loop
/// goes through this function with a *brand new* pipe. That is critical:
/// komorebi keeps its list of subscribers in-memory, so when komorebi
/// restarts (`komorebic stop` / `komorebic start`) our previous subscription
/// is gone. We re-register by calling `subscribe-pipe` again, and we MUST
/// wait for it to finish and check its exit status — otherwise a failure
/// (komorebi still down) goes unnoticed and the `server.connect()` below
/// would hang forever, never giving the next attempt a chance.
async fn subscribe_loop(tx: &UnboundedSender<WorldState>) -> Result<()> {
    let pipe_name = unique_pipe_name();
    let pipe_path = format!("{PIPE_PREFIX}{pipe_name}");

    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_path)
        .with_context(|| format!("create named pipe {pipe_path}"))?;

    tracing::debug!(pipe = %pipe_name, "running `komorebic subscribe-pipe`");
    run_subscribe_pipe(&pipe_name).await?;
    tracing::debug!(pipe = %pipe_name, "subscribed; waiting for komorebi to connect");

    // Wait for komorebi to connect to the pipe. komorebi only opens the pipe
    // when it has an event to deliver, which on an idle desktop can take a
    // long time. Race `server.connect()` against a periodic health-check so
    // we don't wait forever if komorebi disappeared right after subscribing.
    //
    // Wrapped in a scope so `connect_fut`'s borrow of `server` ends before
    // we move `server` into the `BufReader` below.
    {
        let started = Instant::now();
        let connect_fut = server.connect();
        tokio::pin!(connect_fut);
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            if started.elapsed() >= CONNECT_MAX_WAIT {
                tracing::debug!(
                    waited_s = started.elapsed().as_secs(),
                    "no client connected; recycling subscription"
                );
                return Ok(());
            }
            tokio::select! {
                r = &mut connect_fut => {
                    r.context("wait for komorebic to connect to the named pipe")?;
                    break;
                }
                _ = sleep(HEALTH_INTERVAL) => {
                    match tokio::time::timeout(KOMOREBIC_TIMEOUT, fetch_state()).await {
                        Ok(Ok(state)) => {
                            if tx.send(state).is_err() {
                                return Ok(());
                            }
                        }
                        Ok(Err(e)) => {
                            anyhow::bail!(
                                "komorebi appears unavailable while waiting for pipe connection: {e:#}"
                            );
                        }
                        Err(_) => {
                            anyhow::bail!(
                                "`komorebic state` timed out while waiting for pipe connection"
                            );
                        }
                    }
                }
            }
        }
    }
    tracing::debug!("komorebi connected to pipe");

    let reader = BufReader::new(server);
    let mut lines = reader.lines();

    // Debounce: after an event, defer the state fetch by `DEBOUNCE`. If
    // another event arrives during the wait, push the deadline forward.
    let mut deadline: Option<Instant> = None;

    loop {
        if tx.is_closed() {
            return Ok(());
        }

        let next = if let Some(d) = deadline {
            let remaining = d.saturating_duration_since(Instant::now());
            tokio::select! {
                line = lines.next_line() => ReadOutcome::Line(line),
                _ = sleep(remaining) => ReadOutcome::DebounceElapsed,
            }
        } else {
            ReadOutcome::Line(lines.next_line().await)
        };

        match next {
            ReadOutcome::DebounceElapsed => {
                deadline = None;
                match fetch_state().await {
                    Ok(state) => {
                        if tx.send(state).is_err() {
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "komorebic state failed mid-stream");
                    }
                }
            }
            ReadOutcome::Line(Ok(Some(line))) => {
                tracing::trace!(bytes = line.len(), "komorebi event received");
                deadline = Some(Instant::now() + DEBOUNCE);
            }
            ReadOutcome::Line(Ok(None)) => {
                // EOF: komorebi closed its side of the pipe. Most likely it
                // was stopped (`komorebic stop`) or crashed; the outer loop
                // will back off and re-subscribe with a fresh pipe.
                return Ok(());
            }
            ReadOutcome::Line(Err(e)) => {
                return Err(anyhow::Error::from(e).context("read from komorebi pipe"));
            }
        }
    }
}

/// Run `komorebic subscribe-pipe <name>` and wait for it to finish.
///
/// This is a short-lived command: it sends an IPC message to komorebi
/// asking it to register the given pipe in its in-memory subscriber list,
/// then exits. The exit status is the only signal we get about whether the
/// subscription actually took effect — so we MUST observe it. A non-zero
/// exit almost always means komorebi is not running (yet).
async fn run_subscribe_pipe(pipe_name: &str) -> Result<()> {
    let status = tokio::time::timeout(
        KOMOREBIC_TIMEOUT,
        komorebic(&["subscribe-pipe", pipe_name])
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .context("`komorebic subscribe-pipe` timed out")?
    .context("run `komorebic subscribe-pipe` (is komorebic on PATH?)")?;

    if !status.success() {
        anyhow::bail!(
            "`komorebic subscribe-pipe` exited with {status} (is komorebi running?)"
        );
    }
    Ok(())
}

enum ReadOutcome {
    Line(std::io::Result<Option<String>>),
    DebounceElapsed,
}

/// Run `komorebic state` once and parse the JSON into a [`WorldState`].
pub async fn fetch_state() -> Result<WorldState> {
    let output = komorebic(&["state"])
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .context("run `komorebic state` (is komorebic on PATH?)")?;

    if !output.status.success() {
        anyhow::bail!(
            "`komorebic state` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let raw: types::State = serde_json::from_slice(&output.stdout)
        .context("parse `komorebic state` JSON")?;
    Ok(WorldState::from(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_unique_and_well_formed() {
        let a = unique_pipe_name();
        let b = unique_pipe_name();
        assert_ne!(a, b);
        assert!(a.starts_with("komorebi-tray-grid-"));
        // No backslashes; komorebic only takes the suffix.
        assert!(!a.contains('\\'));
        assert!(!a.contains('/'));
    }
}
