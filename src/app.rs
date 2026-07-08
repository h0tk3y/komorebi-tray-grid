//! Top-level event-loop application logic.
//!
//! Owns the shared context menu, the live [`TrayManager`], the latest
//! [`WorldState`], and the autostart toggle state. Every interaction
//! ultimately routes through one of the `on_*` handlers, which are driven by
//! the `tao` event loop in `main.rs`.

use std::collections::HashMap;

use anyhow::Result;
use komorebi_client::SocketMessage;
use tao::event_loop::ControlFlow;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};

use crate::komorebi::client::send_command;
use crate::komorebi::state::{MonitorState, WorldState};
use crate::render::Theme;
use crate::tray::TrayManager;
use crate::utils::ellipsize;

/// The full event-loop-bound application.
pub struct App {
    world: WorldState,
    tray: TrayManager,
    autostart_item_id: MenuId,
    quit_item_id: MenuId,
    initial_autostart: bool,
    max_title_length: usize,
    max_combined_title_length: usize,
    /// Maps menu item ID to (stable_monitor_id, workspace_index)
    workspace_items: HashMap<MenuId, (String, usize)>,
    /// Instant when the menu was last opened, used to defer updates
    /// that might dismiss the menu.
    menu_opened_at: Option<std::time::Instant>,
    /// Buffers the latest world state received while the menu was open.
    pending_world: Option<WorldState>,
}

impl App {
    /// Build the [`TrayManager`] and seed the initial autostart state.
    pub fn new(
        initial_autostart: bool,
        theme: Theme,
        max_title_length: usize,
        max_combined_title_length: usize,
    ) -> Result<Self> {
        Ok(Self {
            world: WorldState::default(),
            tray: TrayManager::new(theme),
            autostart_item_id: MenuId::new("autostart"),
            quit_item_id: MenuId::new("quit"),
            initial_autostart,
            max_title_length,
            max_combined_title_length,
            workspace_items: HashMap::new(),
            menu_opened_at: None,
            pending_world: None,
        })
    }

    /// Apply a fresh komorebi snapshot.
    pub fn on_state_changed(&mut self, new_state: WorldState) {
        if new_state == self.world {
            return;
        }

        // While a context menu is open we must not rebuild the tray icons: the
        // menu is a modal `TrackPopupMenu` loop and a re-entrant reconcile would
        // dismiss the popup. Buffer the newest state instead; it is flushed the
        // moment the menu closes (see `show_menu_for_monitor`). The elapsed-time
        // check is only a safety net in case a close signal is ever missed.
        if let Some(opened_at) = self.menu_opened_at {
            if opened_at.elapsed() < std::time::Duration::from_secs(10) {
                tracing::debug!("buffering state change while menu is active");
                self.pending_world = Some(new_state);
                return;
            } else {
                tracing::warn!("menu lock exceeded safety timeout; forcing flush");
                self.clear_menu_lock();
            }
        }

        self.world = new_state;
        self.reconcile_tray();
    }

    pub fn on_theme_changed(&mut self, theme: Theme) {
        self.workspace_items.clear();
        let mut workspace_items = HashMap::new();
        let max_title_length = self.max_title_length;
        let max_combined_title_length = self.max_combined_title_length;
        let world = self.world.clone();
        if let Err(e) = self.tray.set_theme(theme, &world, |m| {
            build_menu_for_monitor(
                m,
                &mut workspace_items,
                self.autostart_item_id.clone(),
                self.quit_item_id.clone(),
                self.initial_autostart,
                max_title_length,
                max_combined_title_length,
            )
        }) {
            tracing::error!(error = %e, "failed to apply updated theme");
        } else {
            self.workspace_items = workspace_items;
            tracing::debug!("applied updated theme");
        }
    }

    /// React to a tray-icon interaction by opening the corresponding monitor's
    /// context menu ourselves.
    ///
    /// We disabled `tray-icon`'s built-in auto-open (see `TrayManager`) so the
    /// popup goes through [`Self::show_menu_for_monitor`], which blocks until the
    /// menu closes and therefore gives us a precise "menu dismissed" signal.
    /// Open on a left- or right-button release, mirroring the previous default.
    pub fn on_tray_event(&mut self, event: tray_icon::TrayIconEvent) {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
        if let TrayIconEvent::Click {
            id,
            button: MouseButton::Left | MouseButton::Right,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            if let Some(monitor_id) = self.tray.monitor_id_for_tray(&id) {
                self.show_menu_for_monitor(&monitor_id);
            }
        }
    }

    fn reconcile_tray(&mut self) {
        let mut workspace_items = HashMap::new();
        let max_title_length = self.max_title_length;
        let max_combined_title_length = self.max_combined_title_length;
        let world = self.world.clone();
        if let Err(e) = self.tray.reconcile(&world, |m| {
            build_menu_for_monitor(
                m,
                &mut workspace_items,
                self.autostart_item_id.clone(),
                self.quit_item_id.clone(),
                self.initial_autostart,
                max_title_length,
                max_combined_title_length,
            )
        }) {
            tracing::error!(error = %e, "tray reconcile failed");
        } else {
            self.workspace_items = workspace_items;
            tracing::debug!(
                monitors = self.world.monitors.len(),
                icons = self.tray.len(),
                "tray state updated",
            );
        }
    }

    /// Clear the menu open lock and apply any pending state.
    pub fn clear_menu_lock(&mut self) {
        self.menu_opened_at = None;
        crate::tray::MENU_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(pending) = self.pending_world.take() {
            tracing::debug!("applying pending state after menu lock cleared");
            self.world = pending;
            self.reconcile_tray();
        }
    }
}

/// Parse a workspace menu-item id back into its stable monitor identity and
/// workspace index.
///
/// New ids use a hex-encoded monitor id so the selection can be resolved
/// against the current komorebi world even if the monitor order changes while
/// the menu is open. Older numeric ids are still accepted for compatibility.
fn parse_workspace_menu_id(id: &str) -> Option<(String, usize)> {
    let rest = id.strip_prefix("ws-")?;
    let (monitor, workspace) = rest.split_once('-')?;

    if let Some(encoded) = monitor.strip_prefix('h') {
        return Some((decode_hex(encoded)?, workspace.parse().ok()?));
    }

    Some((monitor.to_string(), workspace.parse().ok()?))
}

fn encode_monitor_id(monitor_id: &str) -> String {
    let mut out = String::with_capacity(monitor_id.len() * 2);
    for byte in monitor_id.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

fn decode_hex(input: &str) -> Option<String> {
    if input.len() % 2 != 0 {
        return None;
    }

    let mut bytes = Vec::with_capacity(input.len() / 2);
    let mut iter = input.as_bytes().chunks_exact(2);
    for pair in &mut iter {
        let hi = (pair[0] as char).to_digit(16)? as u8;
        let lo = (pair[1] as char).to_digit(16)? as u8;
        bytes.push((hi << 4) | lo);
    }

    String::from_utf8(bytes).ok()
}

fn build_menu_for_monitor(
    monitor: &MonitorState,
    workspace_items: &mut HashMap<MenuId, (String, usize)>,
    autostart_item_id: MenuId,
    quit_item_id: MenuId,
    initial_autostart: bool,
    max_title_length: usize,
    max_combined_title_length: usize,
) -> Menu {
    let menu = Menu::new();

    // Workspaces
    for ws in &monitor.menu_workspaces {
        if !ws.focused && ws.window_titles.is_empty() {
            continue;
        }

        let digit = ws.index + 1;
        let base_label = format!("&{}.", digit);

        let label = if ws.window_titles.is_empty() {
            base_label
        } else {
            let titles_joined = ws
                .window_titles
                .iter()
                .map(|t| ellipsize(t, max_title_length))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{} {}",
                base_label,
                ellipsize(&titles_joined, max_combined_title_length)
            )
        };

        // Use the native Windows menu-item marker (a checkmark) to indicate the
        // focused workspace, rather than drawing our own Unicode glyph.
        let item = CheckMenuItem::with_id(
            MenuId::new(format!(
                "ws-h{}-{}",
                encode_monitor_id(&monitor.id),
                ws.index
            )),
            label,
            true,
            ws.focused,
            None,
        );
        workspace_items.insert(item.id().clone(), (monitor.id.clone(), ws.index));
        let _ = menu.append(&item);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    let autostart_item = CheckMenuItem::with_id(
        autostart_item_id,
        "&Enable autostart",
        true,
        initial_autostart,
        None,
    );
    let _ = menu.append(&autostart_item);

    let quit_item = MenuItem::with_id(quit_item_id, "&Quit", true, None);
    let _ = menu.append(&quit_item);

    menu
}

impl App {
    /// Handle a menu activation.
    pub fn on_menu_event(
        &mut self,
        event: MenuEvent,
        control_flow: &mut ControlFlow,
        on_autostart_toggle: &mut dyn FnMut(bool) -> bool,
        is_autostart_enabled: &dyn Fn() -> bool,
    ) {
        // Any menu interaction (including those not handled here) should
        // clear the lock and flush state.
        self.clear_menu_lock();

        if event.id == self.quit_item_id {
            tracing::info!("quit requested via tray menu");
            self.tray.clear();
            *control_flow = ControlFlow::Exit;
            return;
        }

        if event.id == self.autostart_item_id {
            let requested = !is_autostart_enabled();
            let applied = on_autostart_toggle(requested);
            self.initial_autostart = applied;
            // We need to rebuild menus to reflect the new checkmark state
            self.reconcile_tray();
            tracing::info!(enabled = applied, "autostart toggle handled");
            return;
        }

        // Resolve the clicked workspace. Prefer the map populated when the menu
        // was built, but fall back to parsing the deterministic id: reconciling
        // the tray on close rebuilds `workspace_items`, and if the buffered
        // state no longer renders that workspace (e.g. it became empty) the map
        // entry disappears even though the user did click it.
        if let Some((monitor_id, ws_idx)) = self
            .workspace_items
            .get(&event.id)
            .cloned()
            .or_else(|| parse_workspace_menu_id(event.id.as_ref()))
        {
            if let Some(monitor_index) = self.world.monitors.iter().position(|m| m.id == monitor_id)
            {
                tracing::info!(
                    monitor = %monitor_id,
                    monitor_index,
                    workspace = ws_idx,
                    "switching workspace via menu"
                );
                let msg = SocketMessage::FocusMonitorWorkspaceNumber(monitor_index, ws_idx);
                if let Err(e) = send_command(msg) {
                    tracing::error!(error = %e, "failed to send focus command");
                }
            } else {
                tracing::warn!(
                    monitor = %monitor_id,
                    workspace = ws_idx,
                    "clicked workspace menu item but the monitor no longer exists"
                );
            }
        }
    }

    pub fn show_menu_for_monitor_index(&mut self, index: usize) {
        if let Some(monitor) = self.world.monitors.get(index) {
            let monitor_id = monitor.id.clone();
            self.show_menu_for_monitor(&monitor_id);
        }
    }

    /// Open the context menu for the monitor with the given id.
    ///
    /// On Windows `TrayManager::show_menu` blocks inside `TrackPopupMenu` until
    /// the popup is dismissed. We pause reconciliation for the duration (so a
    /// re-entrant state update can't rebuild the icons and dismiss the menu),
    /// then flush any buffered state the instant it closes — no timeout needed.
    fn show_menu_for_monitor(&mut self, monitor_id: &str) {
        self.menu_opened_at = Some(std::time::Instant::now());
        crate::tray::MENU_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Err(e) = self.tray.show_menu(monitor_id) {
            tracing::error!(error = %e, monitor_id = %monitor_id, "failed to show tray menu");
        }
        // Control returns here only once the popup has closed.
        self.clear_menu_lock();
    }

    pub fn monitor_count(&self) -> usize {
        self.world.monitors.len()
    }

    /// Index of the monitor komorebi currently reports as focused, if any.
    pub fn active_monitor_index(&self) -> Option<usize> {
        self.world.monitors.iter().position(|m| m.active)
    }

    /// World-state monitor indices ordered left to right by their `x`
    /// coordinate. Ties (e.g. missing coordinates) keep komorebi's order.
    pub fn monitors_left_to_right(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.world.monitors.len()).collect();
        order.sort_by_key(|&i| self.world.monitors[i].x);
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::komorebi::state::{MonitorState, WorkspaceMenuState, WorldState};
    use crate::render::Theme;

    #[test]
    fn test_empty_workspaces_filtered_except_focused() {
        let mut workspace_items = HashMap::new();
        let autostart_id = MenuId::new("autostart");
        let quit_id = MenuId::new("quit");

        let monitor = MonitorState {
            id: "m1".into(),
            menu_workspaces: vec![
                WorkspaceMenuState {
                    index: 0,
                    focused: false,
                    window_titles: vec!["Win1".into()],
                },
                WorkspaceMenuState {
                    index: 1,
                    focused: false,
                    window_titles: vec![],
                }, // Should be filtered
                WorkspaceMenuState {
                    index: 2,
                    focused: true,
                    window_titles: vec![],
                }, // Focused, should NOT be filtered
                WorkspaceMenuState {
                    index: 3,
                    focused: false,
                    window_titles: vec!["Win2".into()],
                },
            ],
            ..Default::default()
        };

        let menu = build_menu_for_monitor(
            &monitor,
            &mut workspace_items,
            autostart_id,
            quit_id,
            false,
            64,
            96,
        );

        // Expected items: WS 0, WS 2 (focused), separator, autostart, quit, WS 3.
        // Wait, WS 3 is also there.
        // So: WS 0, WS 2, WS 3.

        let items = menu.items();
        let labels: Vec<String> = items
            .iter()
            .filter_map(|i| {
                if let Some(m) = i.as_menuitem() {
                    Some(m.text())
                } else if let Some(c) = i.as_check_menuitem() {
                    Some(c.text())
                } else {
                    None
                }
            })
            .collect();

        // The focused-workspace indicator is now the native menu checkmark
        // (driven by CheckMenuItem's checked state), so labels no longer carry
        // a glyph prefix.
        // WS 0 (index 0) -> "&1. Win1"
        // WS 1 (index 1) -> filtered
        // WS 2 (index 2) -> "&3." (focused, natively checked)
        // WS 3 (index 3) -> "&4. Win2"

        assert!(labels.contains(&"&1. Win1".to_string()));
        assert!(!labels.iter().any(|l| l.contains("&2.")));
        assert!(labels.contains(&"&3.".to_string()));
        assert!(labels.contains(&"&4. Win2".to_string()));

        // Check that workspace_items map also only contains what's in the menu
        assert_eq!(workspace_items.len(), 3);
        assert!(workspace_items.values().any(|&(_, ws_idx)| ws_idx == 0));
        assert!(workspace_items.values().any(|&(_, ws_idx)| ws_idx == 2));
        assert!(workspace_items.values().any(|&(_, ws_idx)| ws_idx == 3));
        assert!(!workspace_items.values().any(|&(_, ws_idx)| ws_idx == 1));
    }

    #[test]
    fn test_parse_workspace_menu_id() {
        // Ids built by `build_menu_for_monitor` round-trip back to stable
        // monitor ids and workspace indices.
        assert_eq!(
            parse_workspace_menu_id("ws-h6d312d61-0"),
            Some(("m1-a".into(), 0))
        );
        assert_eq!(
            parse_workspace_menu_id("ws-h4445562d31-3"),
            Some(("DEV-1".into(), 3))
        );
        // Older numeric ids are still accepted for compatibility.
        assert_eq!(parse_workspace_menu_id("ws-12-7"), Some(("12".into(), 7)));
        // Non-workspace ids are ignored.
        assert_eq!(parse_workspace_menu_id("autostart"), None);
        assert_eq!(parse_workspace_menu_id("quit"), None);
        assert_eq!(parse_workspace_menu_id("ws-1"), None);
        assert_eq!(parse_workspace_menu_id("ws-a-b"), None);
    }

    #[test]
    fn test_encode_monitor_id_round_trips_through_workspace_menu_id() {
        let encoded = encode_monitor_id("DEV-1");
        let id = format!("ws-h{encoded}-4");
        assert_eq!(parse_workspace_menu_id(&id), Some(("DEV-1".into(), 4)));
    }

    #[test]
    fn test_monitors_ordered_left_to_right_by_x() {
        let mut app = App::new(false, Theme::default(), 64, 96).unwrap();
        // Provided in a non-left-to-right order: x = 1920, 0, 3840.
        app.on_state_changed(WorldState {
            monitors: vec![
                MonitorState {
                    id: "middle".into(),
                    x: 1920,
                    ..Default::default()
                },
                MonitorState {
                    id: "left".into(),
                    x: 0,
                    ..Default::default()
                },
                MonitorState {
                    id: "right".into(),
                    x: 3840,
                    ..Default::default()
                },
            ],
        });

        // Expect world indices reordered by ascending x: left(1), middle(0), right(2).
        assert_eq!(app.monitors_left_to_right(), vec![1, 0, 2]);
    }

    #[test]
    fn test_state_buffering_and_flush() {
        let mut app = App::new(false, Theme::default(), 64, 96).unwrap();
        let state1 = WorldState {
            monitors: vec![MonitorState {
                id: "m1".into(),
                ..Default::default()
            }],
        };
        let state2 = WorldState {
            monitors: vec![MonitorState {
                id: "m2".into(),
                ..Default::default()
            }],
        };

        // Normal update
        app.on_state_changed(state1.clone());
        assert_eq!(app.world, state1);

        // Simulate an open menu. In production the lock is set inside
        // `show_menu_for_monitor`, but that call blocks on the real Win32
        // `TrackPopupMenu`, so here we set the lock directly to exercise the
        // buffering/flush logic in isolation.
        app.menu_opened_at = Some(std::time::Instant::now());
        crate::tray::MENU_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);

        // Update while menu open
        app.on_state_changed(state2.clone());
        assert_eq!(app.world, state1, "State should be buffered, not applied");
        assert_eq!(app.pending_world, Some(state2.clone()));

        // Flush via menu event
        app.clear_menu_lock();
        assert_eq!(
            app.world, state2,
            "Buffered state should be applied after clear_menu_lock"
        );
        assert_eq!(app.pending_world, None);
    }
}
