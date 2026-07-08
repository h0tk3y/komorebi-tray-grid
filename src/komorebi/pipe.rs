//! Worker that subscribes to komorebi via the [`komorebi_client`] IPC API
//! and forwards [`WorldState`] snapshots over an mpsc channel.
//!
//! Protocol — important and not entirely obvious:
//!
//! - A subscriber binds an AF_UNIX listener at a known path under komorebi's
//!   `DATA_DIR` (`%LOCALAPPDATA%\komorebi`) and registers it with
//!   `SocketMessage::AddSubscriberSocket(name)` — that's what
//!   [`komorebi_client::subscribe`] does in one shot.
//! - **For every notification**, komorebi opens a brand-new
//!   `UnixStream::connect(path)`, writes the JSON-serialized
//!   `Notification { event, state }` payload, and **closes the stream**.
//!   In other words: the subscriber sees one accepted connection per event,
//!   not a long-lived NDJSON stream.
//! - The notification carries the full `State` already, so we never need to
//!   round-trip back to komorebi with `SocketMessage::State` on the steady-
//!   state path. We still use it to seed the initial UI and to refresh after
//!   a reconnect.
//! - komorebi keys its subscriber registry by name (`HashMap<name, path>`),
//!   so re-registering with the same name is idempotent. We use a stable,
//!   per-PID name and a stable listener for the lifetime of the worker.
//!
//! ### Recovering from komorebi restarts
//!
//! komorebi keeps its `SUBSCRIPTION_SOCKETS` registry **in-process memory**
//! and writes it nowhere — so when the user runs `komorebic stop && komorebic
//! start` (or komorebi crashes and restarts), our subscription is forgotten
//! the moment the daemon exits, even though our `UnixListener` is still
//! bound to the same path on disk. There is no portable way to interrupt
//! the blocking `UnixListener::accept` on Windows, so a passive listener
//! can never notice that komorebi has gone away.
//!
//! Earlier attempts tried to be too clever: keep one long-lived listener for
//! the lifetime of the worker and merely send `SocketMessage::AddSubscriberSocket`
//! after each detected `down → up` transition. That doesn't actually work
//! in practice — partly because of `accept()` not being interruptible (the
//! worker can sit blocked on a doomed listener long after komorebi has come
//! back), partly because there are edge cases (komorebi marking us stale
//! and `remove_file`-ing our subscription path during its previous lifetime;
//! the listener's underlying socket being in a weird state after the peer
//! daemon crashed) where the listener is no longer wired up to anything on
//! komorebi's side even after a clean `AddSubscriberSocket` round-trip.
//!
//! The current design uses the watchdog only to **trigger a full re-
//! subscribe**, which is what komorebi-client itself does on first connect
//! and is the only path we know works end-to-end:
//!
//!   1. A short-interval [`WATCHDOG_INTERVAL`] thread does a bare
//!      `UnixStream::connect` to `komorebi.sock` and immediately drops the
//!      stream. komorebi sees EOF on the next read and exits its per-conn
//!      handler with no error log and (critically) **without going through
//!      the `process_command` path, so no `notify_subscribers` broadcast is
//!      triggered** — other subscribers (yasb, komokana, …) stay quiet.
//!   2. The watchdog tracks `was_alive` (seeded from a real initial probe,
//!      not assumed). On any `down → up` transition it:
//!        a) sets a shared `restart` [`AtomicBool`], and
//!        b) wakes the worker's blocked `accept()` by `UnixStream::connect`-
//!           ing to **our own** subscription socket path. That accept-and-
//!           empty-EOF is harmless — the worker reads zero bytes, checks the
//!           `restart` flag, and bails out of [`subscribe_loop`] with a
//!           [`SessionEnd::WatchdogTriggered`].
//!   3. [`run_worker`] then re-runs [`komorebi_client::subscribe`], which
//!      removes any stale socket file at our path, binds a fresh listener,
//!      and sends `AddSubscriberSocket(name)` for us — bringing us back
//!      into komorebi's `SUBSCRIPTION_SOCKETS` registry. komorebi
//!      synchronously calls `notify_subscribers` for that message (it's
//!      an "override event" in `komorebi/src/lib.rs`), which pushes the
//!      current `State` to our brand-new listener, and we then also
//!      explicitly call [`push_fresh_state`] as a belt-and-braces re-seed.
//!   4. We deliberately treat `SessionEnd::WatchdogTriggered` as a *healthy*
//!      termination: no exponential back-off, no "subscription error"
//!      tracing. This keeps recovery near-instant once komorebi is back.
//!
//! A separate, older problem with this worker was that the previous design
//! treated the per-notification EOF as "the subscription died" and applied
//! exponential back-off before re-subscribing — that is what caused the
//! multi-second latency users saw between komorebi events and tray-icon
//! updates. We still `accept()` in a tight loop on the steady-state path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use komorebi_client::{send_query, subscribe, SocketMessage};
use serde::Deserialize;
use uds_windows::UnixStream;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};

use crate::komorebi::{state::WorldState, types};

/// Initial delay between reconnect attempts when the subscription breaks.
const BACKOFF_MIN: Duration = Duration::from_millis(500);

/// Upper bound on the reconnect backoff delay.
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// A session that stayed up at least this long is considered "healthy" —
/// when it terminates, we reset the reconnect backoff to its minimum.
const HEALTHY_SESSION: Duration = Duration::from_secs(5);

/// How often the watchdog probes komorebi's main socket and, when needed,
/// triggers a re-subscribe. The interval is short to maximise the chance of
/// catching the brief `komorebi.sock`-missing window between `komorebic
/// stop` and `komorebic start` — at >1 s polling a fast restart can land
/// entirely between two probes and we'd never see the down phase. The probe
/// itself is a single AF_UNIX `connect` immediately followed by a `close`
/// (no payload), which komorebi handles via its per-connection `read`
/// hitting EOF — it does NOT reach `process_command` / `notify_subscribers`,
/// so other subscribers see no extra traffic.
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);

/// Stable subscriber-socket name for this process. komorebi keys its
/// in-memory subscriber registry by name, so re-`AddSubscriberSocket` calls
/// with the same name are idempotent (`HashMap::insert`).
///
/// We use a **fixed name** (not including the PID) because:
/// 1. Our app has a single-instance guard, so there is only ever one
///    legitimate instance running.
/// 2. If we crash/restart, we want to overwrite the old entry in komorebi
///    rather than leaving an "orphaned" entry. Orphaned entries in komorebi
///    can become "poison": if they are connectable but not writable, they
///    can block or break notifications for all subsequent subscribers in
///    komorebi's sequential notification loop.
pub fn subscription_name() -> String {
    "komorebi-tray-grid.sock".to_string()
}

/// Full path of our subscriber socket, mirroring komorebi-client's
/// `DATA_DIR.join(name)`. The watchdog needs this to **self-connect** in
/// order to unblock the worker's `accept()` when triggering a re-subscribe.
/// Returned as `Option` so the watchdog can degrade gracefully if
/// `LOCALAPPDATA` is unset.
fn subscriber_socket_path(name: &str) -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("komorebi").join(name))
}

/// Thin envelope that picks out only the `state` field of komorebi's
/// `Notification` JSON. The `event` discriminant is intentionally ignored:
/// we always rebuild [`WorldState`] from the full state snapshot, which is
/// robust to komorebi schema evolution (new event variants would otherwise
/// require keeping our deserializer in lockstep with komorebi).
#[derive(Debug, Deserialize)]
struct NotificationEnvelope {
    #[serde(default)]
    state: types::State,
}

/// Run the worker until the receiver is dropped by the consumer.
///
/// The worker:
/// 1. Pushes an initial [`WorldState`] from `SocketMessage::State` so the
///    tray reflects the current desktop on launch even before komorebi
///    sends any notification.
/// 2. Subscribes to komorebi notifications via a process-stable AF_UNIX
///    socket name under komorebi's `DATA_DIR`.
/// 3. `accept()`s notification connections in a loop; for each, reads the
///    full JSON, extracts `state`, and forwards a fresh [`WorldState`].
/// 4. On any `accept()` / `subscribe()` failure, reconnects with bounded
///    exponential backoff. The backoff resets after a session that stayed
///    alive for at least [`HEALTHY_SESSION`], so a flapping komorebi can
///    never cause a spawn storm.
pub fn run_worker(tx: Sender<WorldState>) -> Result<()> {
    // Seed the UI immediately so the user sees something even if komorebi is
    // briefly unreachable. Query failures here just get logged; the subscribe
    // loop will try again as soon as komorebi comes up.
    if !push_fresh_state(&tx) {
        return Ok(());
    }

    let mut backoff = BACKOFF_MIN;
    loop {
        let started = Instant::now();
        let outcome = subscribe_loop(&tx);
        let session = started.elapsed();

        match outcome {
            Ok(SessionEnd::ReceiverGone) => return Ok(()),
            Ok(SessionEnd::WatchdogTriggered) => {
                // Healthy controlled restart, not an error. Re-seed the UI
                // and re-subscribe immediately with NO backoff: the user is
                // staring at the tray waiting for it to catch up after
                // `komorebic start`, and komorebi is already up by
                // construction (that's what the watchdog detected).
                tracing::info!(
                    session_ms = session.as_millis() as u64,
                    "komorebi restart detected; re-subscribing",
                );
                backoff = BACKOFF_MIN;
                if !push_fresh_state(&tx) {
                    return Ok(());
                }
                continue;
            }
            Err(e) => tracing::warn!(
                error = ?e,
                session_ms = session.as_millis() as u64,
                backoff_ms = backoff.as_millis() as u64,
                "komorebi subscription error; backing off",
            ),
        }

        if session >= HEALTHY_SESSION {
            backoff = BACKOFF_MIN;
        }

        thread::sleep(backoff);
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);

        // Re-sync *after* the backoff so the UI catches up once komorebi
        // comes back, even before the first notification arrives. A send
        // failure means the consumer is gone, so exit.
        if !push_fresh_state(&tx) {
            return Ok(());
        }
    }
}

/// Why a single [`subscribe_loop`] iteration returned.
enum SessionEnd {
    /// The state consumer dropped the receiver — the worker should exit.
    ReceiverGone,
    /// The watchdog detected a komorebi `down → up` transition and asked
    /// the worker to tear down the current subscription so a fresh one can
    /// be created via `komorebi_client::subscribe`. Treated as a *healthy*
    /// session termination by [`run_worker`] (no backoff, no error log).
    WatchdogTriggered,
}

/// Subscribe once, then `accept()` per-notification connections in a loop.
///
/// Per the protocol described in the module docstring, each notification is
/// delivered on a fresh, short-lived AF_UNIX stream. We must NOT treat the
/// per-notification EOF as "subscription dropped" — that's what created the
/// multi-second update lag in the previous implementation. The subscription
/// is only torn down when `accept()` itself fails (e.g. the listener got
/// unbound, or some unrecoverable OS-level error).
fn subscribe_loop(state_tx: &Sender<WorldState>) -> Result<SessionEnd> {
    let name = subscription_name();
    tracing::debug!(socket = %name, "subscribing to komorebi");

    let listener =
        subscribe(&name).with_context(|| format!("subscribe to komorebi (socket {name})"))?;

    tracing::debug!(socket = %name, "subscribed; awaiting notifications");

    // `restart` is shared with the watchdog. When set, the watchdog also
    // self-connects to our subscriber socket to unblock the `accept()`
    // below; the worker then sees the flag and bails out.
    let restart = Arc::new(AtomicBool::new(false));
    // Watchdog needs our own subscriber socket path to be able to wake the
    // blocked `accept()` via a self-connect. The `WatchdogHandle::spawn`
    // is what actually constructs it.
    let _watchdog = WatchdogHandle::spawn(name.clone(), Arc::clone(&restart));

    loop {
        // accept blocks until either komorebi pushes a notification or the
        // watchdog self-connects to wake us up after detecting a restart.
        // There is no portable way to interrupt a blocking
        // `UnixListener::accept` on Windows; the watchdog's self-connect is
        // the explicit wake-up mechanism.
        let (mut stream, _addr) = listener
            .accept()
            .context("accept komorebi notification connection")?;

        // Defensive: never let a hanging connection block our worker (and
        // thus potentially hang komorebi's sequential notification loop).
        // Keep this timeout short: if a peer stalls, failing fast keeps
        // subsequent notifications flowing.
        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));

        // After every accepted connection, check whether the watchdog asked
        // us to bail out. We do this regardless of payload size, because
        // the watchdog's wake-up connect carries zero bytes and a legitimate
        // notification may have raced ahead of the watchdog poke (e.g.
        // komorebi already started broadcasting before we returned).
        if restart.swap(false, Ordering::SeqCst) {
            tracing::debug!("watchdog requested re-subscribe; tearing down current session",);
            return Ok(SessionEnd::WatchdogTriggered);
        }

        // Parse directly from the stream rather than reading to EOF first.
        // This avoids waiting for the writer's close when the full JSON is
        // already available.
        let envelope: NotificationEnvelope = match serde_json::from_reader(&mut stream) {
            Ok(e) => e,
            Err(e) => {
                // Empty/probe connection (watchdog wake-up or stray client).
                if e.is_eof() {
                    continue;
                }

                // Schema drift or short/partial write: fall back to a fresh
                // state query so the UI doesn't go stale on a single bad
                // message.
                tracing::warn!(
                    error = ?e,
                    "failed to parse komorebi notification; falling back to state query",
                );
                if !push_fresh_state(state_tx) {
                    return Ok(SessionEnd::ReceiverGone);
                }
                continue;
            }
        };

        let world = WorldState::from(&envelope.state);
        if state_tx.send(world).is_err() {
            return Ok(SessionEnd::ReceiverGone);
        }
    }
}

/// Send a fresh [`WorldState`] over `tx`. Returns `false` if the consumer
/// dropped the receiver (and the worker should therefore exit).
fn push_fresh_state(tx: &Sender<WorldState>) -> bool {
    match fetch_state() {
        Ok(state) => tx.send(state).is_ok(),
        Err(e) => {
            tracing::warn!(error = ?e, "komorebi state query failed");
            // Query failures aren't fatal; keep the worker alive so it can
            // retry on the next reconnect / notification.
            true
        }
    }
}

/// Query komorebi for its current [`SocketMessage::State`] and project it
/// into a [`WorldState`]. Used to seed the UI on startup and to refresh
/// after a reconnect — never on the per-notification hot path.
pub fn fetch_state() -> Result<WorldState> {
    let raw =
        send_query(&SocketMessage::State).context("query komorebi state (is komorebi running?)")?;
    let parsed: types::State = serde_json::from_str(&raw).context("parse komorebi state JSON")?;
    Ok(WorldState::from(&parsed))
}

/// Path to komorebi's main control socket. Mirrors
/// `komorebi_client`'s internal `DATA_DIR.join("komorebi.sock")`, which
/// resolves to `%LOCALAPPDATA%\komorebi\komorebi.sock` on Windows. Returned
/// as `Option` so we can degrade gracefully if `LOCALAPPDATA` is somehow
/// unset — in that case the watchdog disables itself rather than crashing.
pub fn komorebi_socket_path() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("komorebi").join("komorebi.sock"))
}

/// Liveness probe: open a TCP-like connect to komorebi's main socket and
/// drop it immediately. komorebi accepts the connection, reads zero bytes
/// (we never write), hits EOF, and exits its per-connection handler with
/// no further work — in particular, it does **not** reach
/// `process_command` / `notify_subscribers`, so other subscribers see no
/// extra traffic.
fn komorebi_is_alive(path: &PathBuf) -> bool {
    if system_is_shutting_down() {
        return false;
    }

    match UnixStream::connect(path) {
        Ok(stream) => {
            // Be polite and don't sit in komorebi's read with no data:
            // give it a tiny shutdown hint by dropping immediately.
            drop(stream);
            true
        }
        Err(_) => false,
    }
}

fn system_is_shutting_down() -> bool {
    // During OS shutdown/logoff Windows starts tearing down userland pieces in
    // undefined order. Avoid any reconnect/liveness churn in that phase.
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

/// Owns the watchdog thread. On `Drop`, signals the thread to stop (by
/// dropping the `Sender` half of the stop channel — the watchdog uses
/// `recv_timeout`, so a disconnected channel breaks it out of its sleep
/// immediately) and joins it.
struct WatchdogHandle {
    // `Option` so `Drop` can take the values out without `unsafe`.
    stop_tx: Option<Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WatchdogHandle {
    fn spawn(subscription_name: String, restart: Arc<AtomicBool>) -> Self {
        let socket_path = match komorebi_socket_path() {
            Some(p) => p,
            None => {
                // No LOCALAPPDATA → degrade to no-op watchdog. The
                // subscribe loop still works for as long as komorebi
                // stays up; restart-recovery just won't happen.
                tracing::warn!("LOCALAPPDATA not set; komorebi-restart watchdog disabled",);
                return Self {
                    stop_tx: None,
                    join: None,
                };
            }
        };
        let our_socket_path = match subscriber_socket_path(&subscription_name) {
            Some(p) => p,
            None => {
                tracing::warn!("LOCALAPPDATA not set; komorebi-restart watchdog disabled",);
                return Self {
                    stop_tx: None,
                    join: None,
                };
            }
        };

        let (stop_tx, stop_rx) = channel::<()>();
        let join = thread::Builder::new()
            .name("komorebi-watchdog".into())
            .spawn(move || watchdog_thread(socket_path, our_socket_path, restart, stop_rx))
            .ok();

        if join.is_none() {
            tracing::warn!("failed to spawn komorebi-restart watchdog thread");
        }

        Self {
            stop_tx: Some(stop_tx),
            join,
        }
    }
}

impl Drop for WatchdogHandle {
    fn drop(&mut self) {
        // Dropping the sender disconnects the channel; the watchdog's
        // `recv_timeout` returns `Disconnected` and the thread exits
        // immediately, so the join below is essentially free.
        drop(self.stop_tx.take());
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Watchdog body — see the module docstring for the rationale.
///
/// Probes komorebi's main socket on a tight interval and, on any
/// `down → up` transition, asks the worker to tear down its current
/// subscription and re-subscribe by:
///   1. setting `restart` so the worker bails out of [`subscribe_loop`],
///   2. self-connecting to our own subscriber socket so the worker's
///      blocked `accept()` returns immediately with a zero-byte payload.
fn watchdog_thread(
    komorebi_socket: PathBuf,
    our_subscriber_socket: PathBuf,
    restart: Arc<AtomicBool>,
    stop_rx: std::sync::mpsc::Receiver<()>,
) {
    // Seed `was_alive` from a **real** probe, not the optimistic assumption
    // that subscribe just succeeded. This is important: a komorebi restart
    // could land between our successful `subscribe` and the watchdog's
    // first tick, and an optimistic seed would make us miss that transition
    // entirely (we'd see only `true → true` forever).
    let mut was_alive = komorebi_is_alive(&komorebi_socket);
    loop {
        if system_is_shutting_down() {
            tracing::debug!("system shutdown in progress; stopping watchdog");
            return;
        }

        match stop_rx.recv_timeout(WATCHDOG_INTERVAL) {
            // Owner dropped the sender → session is shutting down.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let now_alive = komorebi_is_alive(&komorebi_socket);
        match (was_alive, now_alive) {
            (true, false) => {
                tracing::info!("komorebi appears to be down; will re-subscribe when it comes back",);
            }
            (false, true) => {
                tracing::info!("komorebi is back up; triggering worker re-subscribe",);
                // 1) Tell the worker to bail out on its next accept().
                restart.store(true, Ordering::SeqCst);
                // 2) Wake the blocked accept() via a self-connect. Errors
                //    here are not fatal — the worker may have already
                //    woken on a real komorebi event and processed the
                //    flag; or our listener may be in a transient state.
                //    Either way, the next probe will reconfirm and the
                //    `run_worker` loop will keep retrying.
                match UnixStream::connect(&our_subscriber_socket) {
                    Ok(stream) => drop(stream),
                    Err(e) => tracing::debug!(
                        error = ?e,
                        path = %our_subscriber_socket.display(),
                        "self-connect to wake worker failed; flag still set",
                    ),
                }
            }
            _ => {}
        }
        was_alive = now_alive;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_name_is_stable_and_well_formed() {
        let a = subscription_name();
        let b = subscription_name();
        // Stable across calls — komorebi treats re-subscribe with the same
        // name as idempotent, so reconnecting must not change the name.
        assert_eq!(a, b);
        assert_eq!(a, "komorebi-tray-grid.sock");
        // komorebi joins the name with its DATA_DIR; the leaf must not
        // contain path separators.
        assert!(!a.contains('\\'));
        assert!(!a.contains('/'));
    }

    #[test]
    fn notification_envelope_parses_state_from_full_notification() {
        // Shape mirrors `komorebi_client::Notification`: `{event, state}`.
        // We only care about `state` and must tolerate any `event` shape.
        let raw = r#"{
            "event": { "WindowManager": { "FocusChange": {} } },
            "state": {
                "monitors": {
                    "elements": [
                        {
                            "device_id": "DEV",
                            "device": "dev",
                            "name": "DISPLAY1",
                            "workspaces": { "elements": [], "focused": 0 }
                        }
                    ],
                    "focused": 0
                },
                "is_paused": false
            }
        }"#;
        let env: NotificationEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.state.monitors.elements.len(), 1);
        assert_eq!(env.state.monitors.elements[0].device_id, "DEV");
    }

    #[test]
    fn notification_envelope_tolerates_unknown_event_variants() {
        // New komorebi versions may add brand-new NotificationEvent variants;
        // since we treat `event` as opaque (we don't deserialize it), we
        // must NOT break on values we've never seen.
        let raw = r#"{
            "event": { "SomethingBrandNew": [1, 2, 3] },
            "state": {}
        }"#;
        let env: NotificationEnvelope = serde_json::from_str(raw).unwrap();
        assert!(env.state.monitors.elements.is_empty());
    }

    #[test]
    fn komorebi_socket_path_is_under_localappdata_komorebi() {
        // The watchdog uses this exact path; it must match komorebi-client's
        // internal `DATA_DIR.join("komorebi.sock")` resolution on Windows.
        // We don't assert on a specific drive letter (CI may differ) — just
        // on the trailing components, which is what the bug class would
        // affect.
        let p = komorebi_socket_path().expect("LOCALAPPDATA must be set on Windows test hosts");
        let s = p.to_string_lossy().to_string();
        assert!(
            s.ends_with(r"\komorebi\komorebi.sock"),
            "unexpected socket path: {s}",
        );
    }

    #[test]
    fn watchdog_handle_drop_terminates_thread_promptly() {
        // Spawn the watchdog (komorebi is almost certainly NOT running in
        // tests, so `komorebi_is_alive` just returns false quickly each tick).
        // Then drop the handle and verify the join completes well within
        // `WATCHDOG_INTERVAL` — the channel-disconnect must wake the thread
        // out of `recv_timeout`, not let it sit idle for the full interval.
        let restart = Arc::new(AtomicBool::new(false));
        let handle = WatchdogHandle::spawn(
            "komorebi-tray-grid-test.sock".to_string(),
            Arc::clone(&restart),
        );
        let started = Instant::now();
        drop(handle);
        let elapsed = started.elapsed();
        assert!(
            elapsed < WATCHDOG_INTERVAL,
            "watchdog drop took {elapsed:?}, expected < {WATCHDOG_INTERVAL:?}",
        );
        // The flag must remain unset: there was no `down → up` to detect
        // (komorebi is not running in the test harness).
        assert!(!restart.load(Ordering::SeqCst));
    }

    #[test]
    fn subscriber_socket_path_uses_data_dir_layout() {
        // The watchdog's self-connect target must live in the same
        // directory komorebi-client itself uses for subscriber sockets
        // (`DATA_DIR.join(name)` == `%LOCALAPPDATA%\komorebi\<name>`).
        let p = subscriber_socket_path("komorebi-tray-grid-test.sock")
            .expect("LOCALAPPDATA must be set on Windows test hosts");
        let s = p.to_string_lossy().to_string();
        assert!(
            s.ends_with(r"\komorebi\komorebi-tray-grid-test.sock"),
            "unexpected subscriber socket path: {s}",
        );
    }
}
