//! komorebi-tray-grid — Windows system tray indicator for komorebi.
//!
//! Wires together:
//! - the `tao` event loop (owns the OS message pump and the tray icons);
//! - the `tray-icon` static event handlers (forwarded into the event loop
//!   as [`UserEvent::TrayIcon`] / [`UserEvent::Menu`]);
//! - a dedicated worker thread running the `komorebi-client`-backed
//!   subscription worker, with another bridge thread forwarding
//!   `WorldState`s into [`UserEvent::StateChanged`].

use std::process;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{menu::MenuEvent, TrayIconEvent};

use komorebi_tray_grid::app::App;
use komorebi_tray_grid::autostart;
use komorebi_tray_grid::config;
use komorebi_tray_grid::event::UserEvent;
use komorebi_tray_grid::komorebi::pipe::run_worker;
use komorebi_tray_grid::single_instance::{self, Acquisition};

fn main() {
    init_logging();
    if let Err(e) = run() {
        tracing::error!(error = ?e, "fatal error");
        eprintln!("komorebi-tray-grid: fatal error: {e:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting up");

    // Single-instance check: hold a per-user named mutex for our lifetime.
    // A second launch will detect the existing mutex and exit silently.
    let _instance_guard = match single_instance::acquire()
        .context("acquire single-instance mutex")?
    {
        Acquisition::Acquired(guard) => guard,
        Acquisition::AlreadyRunning => {
            tracing::info!("another instance is already running; exiting");
            return Ok(());
        }
    };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // tray-icon installs *process-global* event handlers; forward both into
    // the tao event loop via the proxy. These must be installed before any
    // tray icon is built so we don't miss the first events.
    {
        let proxy = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let _ = proxy.send_event(UserEvent::TrayIcon(event));
        }));
    }
    {
        let proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = proxy.send_event(UserEvent::Menu(event));
        }));
    }

    // Spawn the komorebi worker on a dedicated OS thread. The worker uses
    // the (synchronous) `komorebi-client` API directly; we can't call it
    // from the tao event-loop thread (that's a win32 message pump), so we
    // bridge worker → event-loop via a sync mpsc channel.
    let (state_tx, state_rx) = mpsc::channel();
    spawn_komorebi_worker(state_tx).context("spawn komorebi worker thread")?;
    spawn_state_bridge(state_rx, proxy.clone())
        .context("spawn komorebi → event-loop bridge thread")?;

    // The `App` (and its tray icons) must be created on the event-loop
    // thread, so defer construction to the `StartCause::Init` event.
    let theme = config::load_theme();
    let mut app: Option<App> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                let initial_autostart = autostart::is_enabled();
                tracing::debug!(initial_autostart, "initializing App on event-loop thread");
                match App::new(initial_autostart, theme) {
                    Ok(a) => app = Some(a),
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to initialize App");
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::StateChanged(state)) => {
                if let Some(a) = app.as_mut() {
                    a.on_state_changed(state);
                }
            }
            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                if let Some(a) = app.as_mut() {
                    // Persist the requested autostart state to the registry;
                    // the callback returns the actually-applied state so
                    // `App` can resync the checkmark if the write failed.
                    let mut autostart_callback = autostart::set_enabled;
                    a.on_menu_event(menu_event, control_flow, &mut autostart_callback);
                }
            }
            Event::UserEvent(UserEvent::TrayIcon(_)) => {
                // Left/double click are no-ops for v1 per spec.
            }
            Event::LoopDestroyed => {
                tracing::info!("event loop destroyed");
            }
            _ => {}
        }
    });
}

fn spawn_komorebi_worker(
    state_tx: mpsc::Sender<komorebi_tray_grid::komorebi::state::WorldState>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("komorebi-worker".into())
        .spawn(move || {
            if let Err(e) = run_worker(state_tx) {
                tracing::error!(error = ?e, "komorebi worker exited with error");
            }
        })
        .map_err(Into::into)
}

fn spawn_state_bridge(
    state_rx: mpsc::Receiver<komorebi_tray_grid::komorebi::state::WorldState>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("komorebi-state-bridge".into())
        .spawn(move || {
            while let Ok(state) = state_rx.recv() {
                if proxy
                    .send_event(UserEvent::StateChanged(state))
                    .is_err()
                {
                    // The event loop has been dropped — nothing more to do.
                    break;
                }
            }
            tracing::debug!("state bridge thread exiting");
        })
        .map_err(Into::into)
}

fn init_logging() {
    use std::fs::OpenOptions;
    use std::sync::Mutex;

    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_env("KOMOREBI_TRAY_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    let stderr_layer = fmt::layer().with_target(false).with_ansi(false);

    // The binary runs under /SUBSYSTEM:WINDOWS so stderr usually goes nowhere.
    // Setting `KOMOREBI_TRAY_LOG_FILE=<path>` (or just `=1` to use the default
    // location under `%LOCALAPPDATA%`) tees logs to a file for diagnostics.
    let file_path = std::env::var_os("KOMOREBI_TRAY_LOG_FILE").map(|raw| {
        if raw == "1" {
            default_log_path()
        } else {
            std::path::PathBuf::from(raw)
        }
    });

    let file_layer = file_path.as_ref().and_then(|path| {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(|file| {
                fmt::layer()
                    .with_target(false)
                    .with_ansi(false)
                    .with_writer(Mutex::new(file))
            })
    });

    let registry = tracing_subscriber::registry().with(filter).with(stderr_layer);
    let _ = if let Some(layer) = file_layer {
        registry.with(layer).try_init()
    } else {
        registry.try_init()
    };
}

fn default_log_path() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("TEMP").map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("komorebi-tray-grid").join("komorebi-tray-grid.log")
}
