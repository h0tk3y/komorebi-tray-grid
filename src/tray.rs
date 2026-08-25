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
    /// Map from `MonitorState::id` → primary Workspace Menu.
    menus: HashMap<String, Menu>,
    theme: Theme,
}

impl TrayManager {
    /// Build an empty manager.
    pub fn new(theme: Theme) -> Self {
        Self {
            icons: HashMap::new(),
            menus: HashMap::new(),
            theme,
        }
    }

    /// Reconcile the live tray icons against `world`.
    /// `get_menu` is a callback that provides the menu for a given monitor.
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
        self.menus.retain(|id, _| seen.contains(id));

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
            existing.set_menu(Some(Box::new(menu.clone())));
            self.menus.insert(monitor.id.clone(), menu);
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
            .with_menu(Box::new(menu.clone()))
            .with_tooltip(tooltip)
            .with_icon(icon)
            .build()
            .context("create tray icon")?;
        self.icons.insert(monitor.id.clone(), tray);
        self.menus.insert(monitor.id.clone(), menu);
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

    /// Show the primary context menu for the given monitor ID.
    pub fn show_menu(&self, monitor_id: &str) -> Result<()> {
        if let Some(menu) = self.menus.get(monitor_id) {
            self.show_menu_internal(monitor_id, menu)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(crate) fn menu(&self, monitor_id: &str) -> Option<&Menu> {
        self.menus.get(monitor_id)
    }

    /// Show a specific menu (e.g. a virtual submenu) for the given monitor ID.
    pub fn show_custom_menu(&self, monitor_id: &str, menu: &Menu) -> Result<()> {
        self.show_menu_internal(monitor_id, menu)
    }

    fn show_menu_internal(&self, monitor_id: &str, menu: &Menu) -> Result<()> {
        if let Some(tray) = self.icons.get_now(monitor_id) {
            // Temporarily attach the requested menu.
            tray.set_menu(Some(Box::new(menu.clone())));
            tray.show_menu();
            // See comments in `show_menu` for why we drain here.
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
