//! Wrapper around `tray-icon` that keeps one icon per komorebi monitor in
//! sync with the latest [`WorldState`].
//!
//! All public methods on this struct **must** be called from the event-loop
//! thread (tray-icon requires this on Windows; see the `tray-icon` README).

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use tray_icon::{
    menu::Menu,
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::komorebi::state::{MonitorState, WorldState};
use crate::render::{paint_monitor_border, render_grid, ICON_SIZE};

/// Manages the lifecycle of all per-monitor tray icons.
pub struct TrayManager {
    /// Shared context menu attached to every tray icon. Cloning a `Menu`
    /// in muda is reference-counted, so all icons see the same items and
    /// the same checkmark state.
    menu: Menu,
    /// Map from `MonitorState::id` → live `TrayIcon` handle. The order
    /// doesn't matter; we look up by id on every reconcile.
    icons: HashMap<String, TrayIcon>,
}

impl TrayManager {
    /// Build an empty manager that will use `menu` for every icon's
    /// right-click context menu.
    pub fn new(menu: Menu) -> Self {
        Self {
            menu,
            icons: HashMap::new(),
        }
    }

    /// Reconcile the live tray icons against `world`:
    /// - existing monitors → update icon image + tooltip;
    /// - new monitors → create a tray icon;
    /// - monitors no longer reported → drop their icon (removes it from
    ///   the system tray when the `TrayIcon` is dropped).
    pub fn reconcile(&mut self, world: &WorldState) -> Result<()> {
        let mut seen: HashSet<String> = HashSet::new();

        // Every icon gets an outer border for a uniform footprint across
        // single- and multi-monitor setups: blue on the active monitor,
        // gray on the inactive ones — see `render::paint_monitor_border`.
        // On a single-monitor setup the only icon is by definition active
        // and thus gets the blue border.
        for monitor in &world.monitors {
            seen.insert(monitor.id.clone());
            self.upsert(monitor, monitor.active)
                .with_context(|| format!("update tray icon for monitor {}", monitor.id))?;
        }

        // Drop icons for monitors that have disappeared.
        self.icons.retain(|id, _| seen.contains(id));

        Ok(())
    }

    /// `active` controls the color of the always-drawn outer monitor
    /// border: `true` → focused (blue), `false` → non-empty (gray).
    fn upsert(&mut self, monitor: &MonitorState, active: bool) -> Result<()> {
        let mut rgba = render_grid(&monitor.cells);
        paint_monitor_border(&mut rgba, active);
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
            return Ok(());
        }

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(self.menu.clone()))
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
}

fn tooltip_for(monitor: &MonitorState) -> String {
    let label = if !monitor.label.is_empty() {
        monitor.label.as_str()
    } else {
        monitor.id.as_str()
    };
    format!("komorebi-tray-grid — {label}")
}
