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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, RegisterHotKey, HOT_KEY_MODIFIERS, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL,
    MOD_SHIFT, MOD_WIN, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
use windows::Win32::Foundation::HWND;

use komorebi_tray_grid::app::App;
use komorebi_tray_grid::autostart;
use komorebi_tray_grid::config;
use komorebi_tray_grid::event::UserEvent;
use komorebi_tray_grid::komorebi::pipe::run_worker;
use komorebi_tray_grid::single_instance::{self, Acquisition};
use komorebi_tray_grid::windows_theme;

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
    let settings = config::load_settings();
    let mut scheme = windows_theme::current_scheme();
    spawn_windows_theme_watcher(proxy.clone()).context("spawn windows theme watcher thread")?;
    let mut app: Option<App> = None;
    let mut current_monitor_index: usize = 0;

    // Register the global hotkey on a dedicated thread with its own message
    // loop. This is crucial: while a tray context menu is open, Windows runs a
    // modal `TrackPopupMenu` loop on the event-loop thread which swallows
    // `WM_HOTKEY` messages. A dedicated thread keeps receiving hotkey presses
    // even while a menu is displayed, so we can dismiss the current menu and
    // cycle to the next monitor immediately.
    if let Some(hotkey_str) = settings.show_hotkey.clone() {
        if let Err(e) = spawn_hotkey_listener(hotkey_str, proxy.clone()) {
            tracing::error!(error = ?e, "failed to spawn hotkey listener thread");
        }
    }

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                let initial_autostart = autostart::is_enabled();
                tracing::debug!(initial_autostart, "initializing App on event-loop thread");
                match App::new(
                    initial_autostart,
                    settings.themes.for_scheme(scheme),
                    settings.max_title_length,
                    settings.max_combined_title_length,
                ) {
                    Ok(a) => app = Some(a),
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to initialize App");
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::Hotkey { menu_was_visible }) => {
                if let Some(a) = app.as_mut() {
                    // Monitors are cycled left to right by their screen
                    // coordinate rather than komorebi's arbitrary reporting
                    // order, so repeated presses walk the displays predictably.
                    let order = a.monitors_left_to_right();
                    if !order.is_empty() {
                        // Predictable behavior: a fresh press (no menu open)
                        // always shows the currently focused monitor. Pressing
                        // the hotkey again while a menu is already visible
                        // advances to the next monitor to the right (wrapping
                        // back to the leftmost).
                        let index = if menu_was_visible {
                            let pos = order
                                .iter()
                                .position(|&i| i == current_monitor_index)
                                .unwrap_or(0);
                            order[(pos + 1) % order.len()]
                        } else {
                            a.active_monitor_index().unwrap_or(order[0])
                        };
                        a.show_menu_for_monitor_index(index);
                        current_monitor_index = index;
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
                    // the App's state will be updated via callbacks.
                    a.on_menu_event(
                        menu_event,
                        control_flow,
                        &mut autostart::set_enabled,
                        &autostart::is_enabled,
                    );
                }
            }
            Event::UserEvent(UserEvent::ColorSchemeChanged(new_scheme)) => {
                if new_scheme != scheme {
                    scheme = new_scheme;
                    if let Some(a) = app.as_mut() {
                        a.on_theme_changed(settings.themes.for_scheme(new_scheme));
                    }
                }
            }
            Event::UserEvent(UserEvent::TrayIcon(tray_event)) => {
                if let Some(a) = app.as_mut() {
                    a.on_tray_event(tray_event);
                }
            }
            Event::LoopDestroyed => {
                tracing::info!("event loop destroyed");
            }
            _ => {}
        }
    });
}

fn spawn_windows_theme_watcher(
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) -> Result<thread::JoinHandle<()>> {
    windows_theme::spawn_watcher(proxy)
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

/// Spawn a dedicated thread that owns the global hotkey registration and runs
/// its own Win32 message loop.
///
/// `RegisterHotKey` delivers `WM_HOTKEY` to the thread that registered it. By
/// registering on a separate thread (rather than the tao event-loop thread),
/// hotkey presses keep arriving even while a tray context menu is open (the
/// event-loop thread is then blocked inside a modal `TrackPopupMenu` loop).
///
/// On each press we forward a [`UserEvent::Hotkey`] to the event loop together
/// with whether a menu was already visible. If one was, we first dismiss it (by
/// injecting an `Esc` key press) and the event loop advances to the next
/// monitor; otherwise the event loop opens the menu for the focused monitor.
fn spawn_hotkey_listener(
    hotkey_str: String,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("hotkey-listener".into())
        .spawn(move || {
            if let Err(e) = register_hotkey(&hotkey_str) {
                tracing::error!(error = %e, "failed to register hotkey");
                return;
            }

            unsafe {
                let mut msg = MSG::default();
                // `GetMessageW` returns 0 on WM_QUIT and -1 on error.
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    if msg.message == WM_HOTKEY {
                        // If one of our tray menus is currently displayed, the
                        // event-loop thread is blocked in a modal popup loop.
                        // Injecting Esc dismisses that popup so the event loop
                        // can resume and show the next monitor's menu. We also
                        // forward whether a menu was visible so the event loop
                        // can decide between the focused monitor and cycling.
                        let menu_was_visible = komorebi_tray_grid::tray::MENU_VISIBLE
                            .load(std::sync::atomic::Ordering::SeqCst);
                        if menu_was_visible {
                            keybd_event(VK_ESCAPE.0 as u8, 0, Default::default(), 0);
                            keybd_event(VK_ESCAPE.0 as u8, 0, KEYEVENTF_KEYUP, 0);
                        }
                        let _ = proxy.send_event(UserEvent::Hotkey { menu_was_visible });
                    }
                }
            }
        })
        .map_err(Into::into)
}

fn register_hotkey(hotkey_str: &str) -> Result<()> {
    let mut modifiers = HOT_KEY_MODIFIERS::default();
    let mut vk = 0u32;

    for part in hotkey_str.split('+') {
        let part = part.trim().to_uppercase();
        match part.as_str() {
            "CTRL" | "CONTROL" => modifiers |= MOD_CONTROL,
            "ALT" | "MENU" => modifiers |= MOD_ALT,
            "SHIFT" => modifiers |= MOD_SHIFT,
            "WIN" | "WINDOWS" | "SUPER" => modifiers |= MOD_WIN,
            s if s.len() == 1 => {
                vk = s.chars().next().unwrap() as u32;
            }
            s => {
                // F1-F24
                if s.starts_with('F') {
                    if let Ok(num) = s[1..].parse::<u32>() {
                        if (1..=24).contains(&num) {
                            vk = 0x6F + num; // F1 is 0x70
                        }
                    }
                }
            }
        }
    }

    if vk == 0 {
        return Err(anyhow::anyhow!("invalid hotkey: {}", hotkey_str));
    }

    unsafe {
        RegisterHotKey(HWND(std::ptr::null_mut()), 1, modifiers, vk).context("RegisterHotKey")?;
    }

    tracing::info!(hotkey = %hotkey_str, "registered global hotkey");
    Ok(())
}
