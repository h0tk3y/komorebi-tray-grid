//! Wrapper around `tray-icon` that keeps one icon per komorebi monitor in
//! sync with the latest [`WorldState`].
//!
//! All public methods on this struct **must** be called from the event-loop
//! thread (tray-icon requires this on Windows; see the `tray-icon` README).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use tray_icon::{menu::Menu, Icon, TrayIcon, TrayIconBuilder, TrayIconId};

use crate::komorebi::state::{MonitorState, WorldState};
use crate::render::{paint_monitor_border_with_theme, render_grid_with_theme, Theme, ICON_SIZE};

/// Tracks whether one of our tray context menus is currently displayed. Set
/// right before we show a menu (via hotkey or tray click) and cleared as soon
/// as the blocking `show_menu` call returns, i.e. the moment the popup closes.
/// Read from the dedicated hotkey listener thread to decide whether an open
/// popup must be dismissed before showing the next monitor's menu.
pub static MENU_VISIBLE: AtomicBool = AtomicBool::new(false);

/// Manages the lifecycle of all per-monitor tray icons.
pub struct TrayManager {
    /// Map from `MonitorState::id` → live `TrayIcon` handle. The order
    /// doesn't matter; we look up by id on every reconcile.
    icons: HashMap<String, TrayIcon>,
    theme: Theme,
}

impl TrayManager {
    /// Build an empty manager.
    pub fn new(theme: Theme) -> Self {
        Self {
            icons: HashMap::new(),
            theme,
        }
    }

    /// Reconcile the live tray icons against `world`.
    /// `get_menu` is a callback that provides a menu for a given monitor.
    pub fn reconcile(
        &mut self,
        world: &WorldState,
        mut get_menu: impl FnMut(&MonitorState) -> Menu,
    ) -> Result<()> {
        let mut seen: HashSet<String> = HashSet::new();

        let mut monitors = world.monitors.clone();
        monitors.sort_by(|a, b| a.id.cmp(&b.id));

        for monitor in &monitors {
            seen.insert(monitor.id.clone());
            let menu = get_menu(monitor);
            self.upsert(monitor, monitor.active, menu)
                .with_context(|| format!("update tray icon for monitor {}", monitor.id))?;
        }

        // Drop icons for monitors that have disappeared.
        self.icons.retain(|id, _| seen.contains(id));

        Ok(())
    }

    pub fn set_theme(
        &mut self,
        theme: Theme,
        world: &WorldState,
        get_menu: impl FnMut(&MonitorState) -> Menu,
    ) -> Result<()> {
        if self.theme == theme {
            return Ok(());
        }
        self.theme = theme;
        self.reconcile(world, get_menu)
    }

    /// `active` controls the color of the always-drawn outer monitor
    /// border: `true` → focused (blue), `false` → non-empty (gray).
    fn upsert(&mut self, monitor: &MonitorState, active: bool, menu: Menu) -> Result<()> {
        let mut rgba = render_grid_with_theme(&monitor.cells, &self.theme);
        paint_monitor_border_with_theme(&mut rgba, active, &self.theme);
        let icon = Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE)
            .context("build tray icon from RGBA buffer")?;
        let tooltip = tooltip_for(monitor);

        if let Some(existing) = self.icons.get_mut(&monitor.id) {
            existing
                .set_icon(Some(icon))
                .context("set tray icon image")?;
            existing
                .set_tooltip(Some(tooltip))
                .context("set tray icon tooltip")?;
            existing.set_menu(Some(Box::new(menu)));
            return Ok(());
        }

        let id = format!("komorebi-tray-grid-{}", monitor.id);
        let tray = TrayIconBuilder::new()
            .with_id(id)
            // We drive the context menu ourselves via `show_menu` so we can
            // observe exactly when it closes (the Win32 `TrackPopupMenu` call
            // blocks until dismissal). Letting `tray-icon` auto-open the menu
            // on click would hide that return point and force us back onto a
            // timeout to detect the close.
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(false)
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip)
            .with_icon(icon)
            .build()
            .context("create tray icon")?;
        self.icons.insert(monitor.id.clone(), tray);
        Ok(())
    }

    /// Drop every tray icon. Useful from the `Quit` handler before the
    /// event loop exits so the system tray cleans up immediately.
    pub fn clear(&mut self) {
        self.icons.clear();
    }

    /// Number of currently visible tray icons.
    pub fn len(&self) -> usize {
        self.icons.len()
    }

    /// `true` if no icons are currently shown.
    pub fn is_empty(&self) -> bool {
        self.icons.is_empty()
    }

    /// Show the context menu for the given monitor ID.
    ///
    /// On Windows this blocks inside `TrackPopupMenu` until the menu is
    /// dismissed, so the caller can treat the return as a precise
    /// "menu closed" signal.
    pub fn show_menu(&self, monitor_id: &str) -> Result<()> {
        if let Some(tray) = self.icons.get_now(monitor_id) {
            tray.show_menu();
            // `tray-icon` invokes `TrackPopupMenu` *without* `TPM_RETURNCMD`, so
            // a menu selection is delivered as a `WM_COMMAND` that is merely
            // *posted* to the tray window's queue and dispatched later by the
            // event loop. Our caller reconciles the tray immediately after this
            // returns, and reconciling replaces (and destroys) the menu that was
            // just shown. If the `WM_COMMAND` is still sitting in the queue at
            // that point, `muda` can no longer resolve the clicked item and the
            // `MenuEvent` is silently dropped — so the workspace switch does
            // nothing. Drain those pending menu messages now, while the menu is
            // still alive, so the selection is always delivered.
            unsafe { drain_pending_menu_messages() };
        }
        Ok(())
    }

    /// Resolve the `MonitorState::id` that owns the given tray icon id, used to
    /// map an incoming tray click back to the monitor whose menu should open.
    pub fn monitor_id_for_tray(&self, tray_id: &TrayIconId) -> Option<String> {
        self.icons
            .iter()
            .find(|(_, icon)| icon.id() == tray_id)
            .map(|(monitor_id, _)| monitor_id.clone())
    }
}

/// Dispatch any `WM_COMMAND` messages currently queued on this (event-loop)
/// thread. Used right after a popup menu closes to force delivery of a menu
/// selection before the menu is reconciled away. Only menu-command messages are
/// pulled from the queue; everything else is left untouched for the normal
/// event loop to handle.
unsafe fn drain_pending_menu_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_COMMAND,
    };
    let mut msg = MSG::default();
    while PeekMessageW(&mut msg, None, WM_COMMAND, WM_COMMAND, PM_REMOVE).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

trait HashMapExt {
    fn get_now(&self, key: &str) -> Option<&TrayIcon>;
}

impl HashMapExt for HashMap<String, TrayIcon> {
    fn get_now(&self, key: &str) -> Option<&TrayIcon> {
        self.get(key)
    }
}

fn tooltip_for(monitor: &MonitorState) -> String {
    let label = if !monitor.label.is_empty() {
        monitor.label.as_str()
    } else {
        monitor.id.as_str()
    };
    format!("komorebi-tray-grid — {label}")
}
