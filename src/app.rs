//! Top-level event-loop application logic.
//!
//! Owns the shared context menu, the live [`TrayManager`], the latest
//! [`WorldState`], and the autostart toggle state. Every interaction
//! ultimately routes through one of the `on_*` handlers, which are driven by
//! the `tao` event loop in `main.rs`.

use std::collections::HashMap;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_DOWN};
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

use anyhow::Result;
use komorebi_client::SocketMessage;
use tao::event_loop::ControlFlow;

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
    workspace_submenus: bool,
    /// Maps menu item ID to (stable_monitor_id, workspace_index)
    workspace_items: HashMap<MenuId, (String, usize)>,
    /// Maps menu item ID to (stable_monitor_id, workspace_index, hwnd)
    window_items: HashMap<MenuId, (String, usize, usize)>,
    /// Maps monitor ID -> (Workspace index -> Window Menu)
    virtual_submenus: HashMap<String, HashMap<usize, Menu>>,
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
        workspace_submenus: bool,
    ) -> Result<Self> {
        Ok(Self {
            world: WorldState::default(),
            tray: TrayManager::new(theme),
            autostart_item_id: MenuId::new("autostart"),
            quit_item_id: MenuId::new("quit"),
            initial_autostart,
            max_title_length,
            max_combined_title_length,
            workspace_submenus,
            workspace_items: HashMap::new(),
            window_items: HashMap::new(),
            virtual_submenus: HashMap::new(),
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
        self.window_items.clear();
        self.virtual_submenus.clear();
        let mut workspace_items = HashMap::new();
        let mut window_items = HashMap::new();
        let mut virtual_submenus = HashMap::new();
        let max_title_length = self.max_title_length;
        let max_combined_title_length = self.max_combined_title_length;
        let workspace_submenus = self.workspace_submenus;
        let world = self.world.clone();
        if let Err(e) = self.tray.set_theme(theme, &world, |m| {
            let (menu, submenus) = build_menu_for_monitor(
                m,
                &mut workspace_items,
                &mut window_items,
                self.autostart_item_id.clone(),
                self.quit_item_id.clone(),
                self.initial_autostart,
                max_title_length,
                max_combined_title_length,
                workspace_submenus,
            );
            virtual_submenus.insert(m.id.clone(), submenus);
            menu
        }) {
            tracing::error!(error = %e, "failed to apply updated theme");
        } else {
            self.workspace_items = workspace_items;
            self.window_items = window_items;
            self.virtual_submenus = virtual_submenus;
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
        use tray_icon::{MouseButtonState, TrayIconEvent};
        if let TrayIconEvent::Click {
            id,
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
        self.workspace_items.clear();
        self.window_items.clear();
        self.virtual_submenus.clear();
        let mut workspace_items = HashMap::new();
        let mut window_items = HashMap::new();
        let mut virtual_submenus = HashMap::new();
        let max_title_length = self.max_title_length;
        let max_combined_title_length = self.max_combined_title_length;
        let workspace_submenus = self.workspace_submenus;
        let world = self.world.clone();
        if let Err(e) = self.tray.reconcile(&world, |m| {
            let (menu, submenus) = build_menu_for_monitor(
                m,
                &mut workspace_items,
                &mut window_items,
                self.autostart_item_id.clone(),
                self.quit_item_id.clone(),
                self.initial_autostart,
                max_title_length,
                max_combined_title_length,
                workspace_submenus,
            );
            virtual_submenus.insert(m.id.clone(), submenus);
            menu
        }) {
            tracing::error!(error = %e, "tray reconcile failed");
        } else {
            self.workspace_items = workspace_items;
            self.window_items = window_items;
            self.virtual_submenus = virtual_submenus;
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

fn parse_window_menu_id(id: &str) -> Option<(String, usize, usize)> {
    let rest = id.strip_prefix("win-")?;
    let mut parts = rest.splitn(3, '-');
    let monitor = parts.next()?;
    let workspace = parts.next()?;
    let window = parts.next()?;

    if let Some(encoded) = monitor.strip_prefix('h') {
        return Some((
            decode_hex(encoded)?,
            workspace.parse().ok()?,
            window.parse().ok()?,
        ));
    }

    Some((
        monitor.to_string(),
        workspace.parse().ok()?,
        window.parse().ok()?,
    ))
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
    window_items: &mut HashMap<MenuId, (String, usize, usize)>,
    autostart_item_id: MenuId,
    quit_item_id: MenuId,
    initial_autostart: bool,
    max_title_length: usize,
    max_combined_title_length: usize,
    workspace_submenus: bool,
) -> (Menu, HashMap<usize, Menu>) {
    let menu = Menu::new();
    let mut virtual_submenus = HashMap::new();

    // 1. Workspace Menu
    for ws in &monitor.menu_workspaces {
        if !ws.focused && ws.windows.is_empty() {
            continue;
        }

        let digit = ws.index + 1;
        let base_label = format!("&{}.", digit);

        let label = if ws.windows.is_empty() {
            base_label
        } else {
            let titles_joined = ws
                .windows
                .iter()
                .map(|w| ellipsize(&w.title, max_title_length))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{} {}",
                base_label,
                ellipsize(&titles_joined, max_combined_title_length)
            )
        };

        let ws_item = CheckMenuItem::with_id(
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
        workspace_items.insert(ws_item.id().clone(), (monitor.id.clone(), ws.index));
        menu.append(&ws_item).unwrap();

        // 2. Virtual Submenu (Window Menu for this workspace)
        if workspace_submenus {
            let win_menu = Menu::new();
            let focus_ws_item = MenuItem::with_id(
                MenuId::new(format!(
                    "focus-ws-h{}-{}",
                    encode_monitor_id(&monitor.id),
                    ws.index
                )),
                "Focus Workspace",
                true,
                None,
            );
            // We reuse the same logic for "Focus Workspace" item by putting it in workspace_items
            workspace_items.insert(focus_ws_item.id().clone(), (monitor.id.clone(), ws.index));
            win_menu.append(&focus_ws_item).unwrap();
            win_menu.append(&PredefinedMenuItem::separator()).unwrap();

            for (win_idx, win) in ws.windows.iter().enumerate() {
                let win_digit = win_idx + 1;
                let win_mnemonic = if win_digit <= 9 {
                    format!("&{}. ", win_digit)
                } else {
                    String::new()
                };

                let win_label = format!("{}{}", win_mnemonic, ellipsize(&win.title, max_title_length));

                let win_item = CheckMenuItem::with_id(
                    MenuId::new(format!(
                        "win-h{}-{}-{}",
                        encode_monitor_id(&monitor.id),
                        ws.index,
                        win_idx
                    )),
                    win_label,
                    true,
                    win.focused,
                    None,
                );
                window_items.insert(
                    win_item.id().clone(),
                    (monitor.id.clone(), ws.index, win.hwnd),
                );
                win_menu.append(&win_item).unwrap();
            }
            virtual_submenus.insert(ws.index, win_menu);
        }
    }

    menu.append(&PredefinedMenuItem::separator()).unwrap();

    let autostart_item = CheckMenuItem::with_id(
        autostart_item_id,
        "&Enable autostart",
        true,
        initial_autostart,
        None,
    );
    menu.append(&autostart_item).unwrap();

    let quit_item = MenuItem::with_id(quit_item_id, "&Quit", true, None);
    menu.append(&quit_item).unwrap();

    (menu, virtual_submenus)
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

        // 1. Check if it's a main workspace item (triggers virtual submenu)
        if event.id.as_ref().starts_with("ws-") {
            if let Some((monitor_id, ws_idx)) = self
                .workspace_items
                .get(&event.id)
                .cloned()
                .or_else(|| parse_workspace_menu_id(event.id.as_ref()))
            {
                if let Some(submenus) = self.virtual_submenus.get(&monitor_id) {
                    if let Some(win_menu) = submenus.get(&ws_idx) {
                        tracing::debug!(monitor = %monitor_id, workspace = ws_idx, "showing virtual submenu");
                        // We must set the lock BEFORE showing the menu
                        self.menu_opened_at = Some(std::time::Instant::now());
                        crate::tray::MENU_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);

                        // Spawn a thread to select the first item ("Focus Workspace")
                        // so it can be triggered by pressing Enter immediately.
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            unsafe {
                                keybd_event(VK_DOWN.0 as u8, 0, Default::default(), 0);
                                keybd_event(VK_DOWN.0 as u8, 0, KEYEVENTF_KEYUP, 0);
                            }
                        });

                        let _ = self.tray.show_custom_menu(&monitor_id, win_menu);
                        self.clear_menu_lock();
                        self.reconcile_tray();
                        return;
                    }
                }

                // Fallback: switch to the workspace (if submenus are disabled or not available)
                if let Some(monitor_index) = self.world.monitors.iter().position(|m| m.id == monitor_id)
                {
                    tracing::info!(
                        monitor = %monitor_id,
                        monitor_index,
                        workspace = ws_idx,
                        "switching workspace"
                    );
                    let msg = SocketMessage::FocusMonitorWorkspaceNumber(monitor_index, ws_idx);
                    if let Err(e) = send_command(msg) {
                        tracing::error!(error = %e, "failed to send focus command");
                    }
                }
                self.reconcile_tray();
                return;
            }
        }

        // 2. Check if it's a focus workspace item (from virtual submenu)
        if event.id.as_ref().starts_with("focus-ws-") {
            let id_str = event.id.as_ref();
            let rest = &id_str["focus-".len()..]; // strip "focus-" to get "ws-..."
            if let Some((monitor_id, ws_idx)) = parse_workspace_menu_id(rest) {
                if let Some(monitor_index) = self.world.monitors.iter().position(|m| m.id == monitor_id)
                {
                    tracing::info!(
                        monitor = %monitor_id,
                        monitor_index,
                        workspace = ws_idx,
                        "switching workspace via virtual submenu"
                    );
                    let msg = SocketMessage::FocusMonitorWorkspaceNumber(monitor_index, ws_idx);
                    if let Err(e) = send_command(msg) {
                        tracing::error!(error = %e, "failed to send focus command");
                    }
                }
                return;
            }
        }

        // 3. Check if it's a window item
        if let Some((monitor_id, ws_idx, win_hwnd)) = self
            .window_items
            .get(&event.id)
            .cloned()
            .or_else(|| {
                let (mid, wid, widx) = parse_window_menu_id(event.id.as_ref())?;
                let hwnd = self
                    .world
                    .monitor(&mid)?
                    .menu_workspaces
                    .get(wid)?
                    .windows
                    .get(widx)?
                    .hwnd;
                Some((mid, wid, hwnd))
            })
        {
            if let Some(monitor_index) = self.world.monitors.iter().position(|m| m.id == monitor_id)
            {
                tracing::info!(
                    monitor = %monitor_id,
                    monitor_index,
                    workspace = ws_idx,
                    hwnd = %win_hwnd,
                    "focusing window via menu"
                );
                // First switch to the monitor and workspace
                let ws_msg = SocketMessage::FocusMonitorWorkspaceNumber(monitor_index, ws_idx);
                let _ = send_command(ws_msg);

                // Then focus the specific window by HWND. Komorebi will see this
                // via its FocusChange event handler and update its state.
                unsafe {
                    let _ = SetForegroundWindow(HWND(win_hwnd as _));
                }
            }
            return;
        }
    }

    pub fn show_menu_for_monitor_index(&mut self, index: usize) {
        if let Some(monitor) = self.world.monitors.get(index) {
            let monitor_id = monitor.id.clone();
            self.show_menu_for_monitor(&monitor_id);
        }
    }

    /// Open the context menu for the monitor with the given id.
    pub fn show_menu_for_monitor(&mut self, monitor_id: &str) {
        self.reconcile_tray();
        self.menu_opened_at = Some(std::time::Instant::now());
        crate::tray::MENU_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Err(e) = self.tray.show_menu(monitor_id) {
            tracing::error!(error = %e, monitor_id = %monitor_id, "failed to show menu");
        }
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
    use crate::komorebi::state::{MonitorState, WindowMenuState, WorkspaceMenuState, WorldState};
    use crate::render::Theme;

    #[test]
    fn test_empty_workspaces_filtered_except_focused() {
        let mut workspace_items = HashMap::new();
        let mut window_items = HashMap::new();
        let autostart_id = MenuId::new("autostart");
        let quit_id = MenuId::new("quit");

        let monitor = MonitorState {
            id: "m1".into(),
            menu_workspaces: vec![
                WorkspaceMenuState {
                    index: 0,
                    focused: false,
                    windows: vec![WindowMenuState {
                        title: "Win1".into(),
                        ..Default::default()
                    }],
                },
                WorkspaceMenuState {
                    index: 1,
                    focused: false,
                    windows: vec![],
                }, // Should be filtered
                WorkspaceMenuState {
                    index: 2,
                    focused: true,
                    windows: vec![],
                }, // Focused, should NOT be filtered
                WorkspaceMenuState {
                    index: 3,
                    focused: false,
                    windows: vec![WindowMenuState {
                        title: "Win2".into(),
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        };

        let (ws_menu, virtual_submenus) = build_menu_for_monitor(
            &monitor,
            &mut workspace_items,
            &mut window_items,
            autostart_id,
            quit_id,
            false,
            64,
            96,
            true,
        );

        // Expected items in ws_menu: WS 0, WS 2 (focused), WS 3, separator, autostart, quit.
        let ws_items = ws_menu.items();
        let ws_labels: Vec<String> = ws_items
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

        assert!(ws_labels.iter().any(|l| l.contains("&1. Win1")));
        assert!(!ws_labels.iter().any(|l| l.contains("&2.")));
        assert!(ws_labels.contains(&"&3.".to_string()));
        assert!(ws_labels.iter().any(|l| l.contains("&4. Win2")));

        // Expected submenus: 0, 2, 3.
        assert_eq!(virtual_submenus.len(), 3);
        let win_menu_0 = virtual_submenus.get(&0).unwrap();
        let win_labels_0: Vec<String> = win_menu_0.items()
            .iter()
            .filter_map(|i| i.as_check_menuitem().map(|m| m.text()))
            .collect();
        assert!(win_labels_0.contains(&"&1. Win1".to_string()));

        // Check that workspace_items map also only contains what's in the menu
        // 3 workspaces * (main item + focus item) = 6 entries.
        assert_eq!(workspace_items.len(), 6);
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
        let mut app = App::new(false, Theme::default(), 64, 96, true).unwrap();
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
        let mut app = App::new(false, Theme::default(), 64, 96, true).unwrap();
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

    #[test]
    fn test_workspace_submenus_disabled() {
        use crate::komorebi::state::WindowMenuState;
        let mut workspace_items = HashMap::new();
        let mut window_items = HashMap::new();
        let autostart_id = MenuId::new("auto");
        let quit_id = MenuId::new("quit");

        let monitor = MonitorState {
            id: "m1".into(),
            menu_workspaces: vec![WorkspaceMenuState {
                index: 0,
                focused: false,
                windows: vec![WindowMenuState {
                    title: "Win1".into(),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        };

        // When disabled
        let (_ws_menu, virtual_submenus) = build_menu_for_monitor(
            &monitor,
            &mut workspace_items,
            &mut window_items,
            autostart_id,
            quit_id,
            false,
            64,
            96,
            false,
        );

        assert_eq!(virtual_submenus.len(), 0, "No virtual submenus should be built");
        
        // The workspace item should still be in workspace_items for fallback focus
        let ws_item_id = MenuId::new(format!("ws-h{}-0", encode_monitor_id("m1")));
        assert!(workspace_items.contains_key(&ws_item_id));
    }

    #[test]
    fn test_workspace_menu_checkmarks_consistent_after_submenu_interaction() {
        let mut app = App::new(false, Theme::default(), 64, 96, true).unwrap();
        let state = WorldState {
            monitors: vec![MonitorState {
                id: "m1".into(),
                menu_workspaces: vec![
                    WorkspaceMenuState {
                        index: 0,
                        focused: true,
                        windows: vec![WindowMenuState {
                            title: "Win1".into(),
                            ..Default::default()
                        }],
                    },
                    WorkspaceMenuState {
                        index: 1,
                        focused: false,
                        windows: vec![WindowMenuState {
                            title: "Win2".into(),
                            ..Default::default()
                        }],
                    },
                ],
                ..Default::default()
            }],
        };

        app.on_state_changed(state);

        // Verify initial checkmark state: WS 0 is checked, WS 1 is unchecked.
        let get_checks = |app: &App| -> Vec<(String, bool)> {
            let menu = app.tray.menu("m1").expect("menu exists");
            menu.items()
                .iter()
                .filter_map(|item| {
                    item.as_check_menuitem()
                        .map(|c| (c.text(), c.is_checked()))
                })
                .collect()
        };

        let initial_checks = get_checks(&app);
        assert_eq!(initial_checks.len(), 3); // WS 0, WS 1, autostart
        assert!(initial_checks[0].1, "WS 0 should be checked initially");
        assert!(!initial_checks[1].1, "WS 1 should be unchecked initially");

        // Simulate user clicking on WS 1
        let ws1_id = MenuId::new(format!("ws-h{}-1", encode_monitor_id("m1")));
        let mut control_flow = ControlFlow::Wait;
        let mut autostart_toggle = |_| true;
        let is_autostart_enabled = || false;

        app.on_menu_event(
            MenuEvent { id: ws1_id },
            &mut control_flow,
            &mut autostart_toggle,
            &is_autostart_enabled,
        );

        // After the submenu interaction is done, the checkmark state MUST remain consistent:
        // WS 0 is still checked, WS 1 is still unchecked.
        let post_checks = get_checks(&app);
        assert!(post_checks[0].1, "WS 0 must remain checked");
        assert!(!post_checks[1].1, "WS 1 must remain unchecked");
    }

    #[test]
    fn test_window_menu_checkmarks_for_active_window() {
        let mut workspace_items = HashMap::new();
        let mut window_items = HashMap::new();
        let autostart_id = MenuId::new("autostart");
        let quit_id = MenuId::new("quit");

        let monitor = MonitorState {
            id: "m1".into(),
            menu_workspaces: vec![WorkspaceMenuState {
                index: 0,
                focused: true,
                windows: vec![
                    WindowMenuState {
                        title: "Win A".into(),
                        hwnd: 100,
                        focused: false,
                        ..Default::default()
                    },
                    WindowMenuState {
                        title: "Win B".into(),
                        hwnd: 200,
                        focused: true,
                        ..Default::default()
                    },
                ],
            }],
            ..Default::default()
        };

        let (_ws_menu, virtual_submenus) = build_menu_for_monitor(
            &monitor,
            &mut workspace_items,
            &mut window_items,
            autostart_id,
            quit_id,
            false,
            64,
            96,
            true,
        );

        let win_menu = virtual_submenus.get(&0).expect("submenu 0 exists");
        let win_checks: Vec<(String, bool)> = win_menu
            .items()
            .iter()
            .filter_map(|i| {
                i.as_check_menuitem()
                    .map(|c| (c.text(), c.is_checked()))
            })
            .collect();

        assert_eq!(win_checks.len(), 2);
        assert_eq!(win_checks[0].0, "&1. Win A");
        assert!(!win_checks[0].1, "Win A should not be checked");
        assert_eq!(win_checks[1].0, "&2. Win B");
        assert!(win_checks[1].1, "Win B should be checked");
    }
}
