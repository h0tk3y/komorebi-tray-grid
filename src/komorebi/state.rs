//! UI-friendly projection of komorebi's [`types::State`] into a per-monitor
//! 3×3 grid of [`render::CellState`]s.

use crate::komorebi::types;
use crate::render::CellState;

/// Number of cells per tray icon (3 × 3).
pub const CELLS_PER_MONITOR: usize = 9;

/// Per-monitor projected state used by the tray manager.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorState {
    /// Stable identifier used as the key when reconciling tray icons across
    /// updates. Falls back from `device_id` → `device` → `name` → a synthetic
    /// `__monitor_<index>__` to handle older / partial komorebi outputs.
    pub id: String,
    /// Human-readable label (used for tray tooltips).
    pub label: String,
    /// `true` when this monitor is the currently focused monitor reported
    /// by komorebi (i.e. matches the outer ring's `focused` index). The
    /// tray manager uses this flag to draw the per-icon active-monitor
    /// border — and only does so when there is more than one monitor.
    pub active: bool,
    /// Nine cells, row-major, top-left first.
    pub cells: [CellState; CELLS_PER_MONITOR],
    /// Workspace metadata used for dynamic tray-menu entries.
    pub menu_workspaces: Vec<WorkspaceMenuState>,
    /// The monitor's `left` coordinate (from komorebi's `size` rect), used to
    /// order monitors from left to right when cycling menus.
    pub x: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceMenuState {
    pub index: usize,
    pub focused: bool,
    pub windows: Vec<WindowMenuState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowMenuState {
    pub title: String,
    pub exe: String,
    pub hwnd: usize,
}

/// Snapshot of every monitor known to komorebi, in komorebi's reported order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorldState {
    pub monitors: Vec<MonitorState>,
}

impl WorldState {
    /// Look up a monitor by its stable id.
    pub fn monitor(&self, id: &str) -> Option<&MonitorState> {
        self.monitors.iter().find(|m| m.id == id)
    }
}

impl From<&types::State> for WorldState {
    fn from(raw: &types::State) -> Self {
        let focused_monitor = raw.monitors.focused;
        let monitors = raw
            .monitors
            .elements
            .iter()
            .enumerate()
            .map(|(i, m)| project_monitor(i, m, i == focused_monitor))
            .collect();
        Self { monitors }
    }
}

impl From<types::State> for WorldState {
    fn from(raw: types::State) -> Self {
        WorldState::from(&raw)
    }
}

fn project_monitor(index: usize, monitor: &types::Monitor, active: bool) -> MonitorState {
    let mut cells = [CellState::EMPTY; CELLS_PER_MONITOR];
    let focused_workspace = monitor.workspaces.focused;

    for (wi, ws) in monitor
        .workspaces
        .elements
        .iter()
        .enumerate()
        .take(CELLS_PER_MONITOR)
    {
        cells[wi] = project_workspace(wi, focused_workspace, ws);
    }

    MonitorState {
        id: monitor_id(index, monitor),
        label: monitor_label(monitor),
        active,
        cells,
        x: monitor.size.left,
        menu_workspaces: monitor
            .workspaces
            .elements
            .iter()
            .enumerate()
            .take(CELLS_PER_MONITOR)
            .map(|(wi, ws)| WorkspaceMenuState {
                index: wi,
                focused: wi == focused_workspace,
                windows: workspace_windows(ws),
            })
            .collect(),
    }
}

fn project_workspace(index: usize, focused_workspace: usize, ws: &types::Workspace) -> CellState {
    let containers = ws.containers.len();
    let floating = ws.floating_windows.len();
    let has_monocle = ws.monocle_container.is_some();
    let has_maximized = ws.maximized_window.is_some();
    let extras = usize::from(has_monocle) + usize::from(has_maximized);
    let window_count = containers.saturating_add(floating).saturating_add(extras);

    CellState {
        focused: index == focused_workspace,
        window_count: window_count.min(u8::MAX as usize) as u8,
        full_screen: has_monocle || has_maximized,
    }
}

fn workspace_windows(ws: &types::Workspace) -> Vec<WindowMenuState> {
    let mut windows = Vec::new();

    for container in &ws.containers.elements {
        for window in &container.windows.elements {
            if let Some(w) = project_window(window) {
                windows.push(w);
            }
        }
    }

    match &ws.floating_windows {
        types::WindowCollection::Ring(ring) => {
            for window in &ring.elements {
                if let Some(w) = project_window(window) {
                    windows.push(w);
                }
            }
        }
        types::WindowCollection::Array(arr) => {
            for window in arr {
                if let Some(w) = project_window(window) {
                    windows.push(w);
                }
            }
        }
        types::WindowCollection::Other(_) => {}
    }

    if let Some(container) = &ws.monocle_container {
        for window in &container.windows.elements {
            if let Some(w) = project_window(window) {
                windows.push(w);
            }
        }
    }

    if let Some(window) = &ws.maximized_window {
        if let Some(w) = project_window(window) {
            windows.push(w);
        }
    }

    windows
}

fn project_window(window: &types::Window) -> Option<WindowMenuState> {
    let title = window.title.trim();
    if title.is_empty() {
        None
    } else {
        Some(WindowMenuState {
            title: title.to_string(),
            exe: window.exe.clone(),
            hwnd: window.hwnd,
        })
    }
}

fn monitor_id(index: usize, monitor: &types::Monitor) -> String {
    if !monitor.device_id.is_empty() {
        monitor.device_id.clone()
    } else if !monitor.device.is_empty() {
        format!("device:{}", monitor.device)
    } else if !monitor.name.is_empty() {
        format!("name:{}", monitor.name)
    } else {
        format!("__monitor_{index}__")
    }
}

fn monitor_label(monitor: &types::Monitor) -> String {
    if !monitor.name.is_empty() {
        monitor.name.clone()
    } else if !monitor.device.is_empty() {
        monitor.device.clone()
    } else {
        monitor.device_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> WorldState {
        let raw: types::State = serde_json::from_value(value).unwrap();
        WorldState::from(&raw)
    }

    #[test]
    fn empty_state_yields_no_monitors() {
        let w = parse(json!({ "monitors": { "elements": [], "focused": 0 } }));
        assert!(w.monitors.is_empty());
    }

    #[test]
    fn workspace_with_containers_is_non_empty_but_not_full_screen() {
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [{
                        "containers": { "elements": [{}], "focused": 0 },
                        "floating_windows": { "elements": [], "focused": 0 },
                        "monocle_container": null,
                        "maximized_window": null
                    }],
                    "focused": 0
                }
            }], "focused": 0 }
        }));
        let m = &w.monitors[0];
        assert_eq!(m.id, "DEV-1");
        assert_eq!(m.cells[0].window_count, 1);
        assert!(m.cells[0].focused);
        assert!(!m.cells[0].full_screen);
        // Trailing cells are empty.
        for i in 1..9 {
            assert_eq!(m.cells[i], CellState::EMPTY, "cell {i} should be empty");
        }
    }

    #[test]
    fn maximized_window_marks_full_screen_and_non_empty() {
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [{
                        "containers": { "elements": [], "focused": 0 },
                        "floating_windows": { "elements": [], "focused": 0 },
                        "maximized_window": { "hwnd": 1 }
                    }],
                    "focused": 0
                }
            }], "focused": 0 }
        }));
        let cell = w.monitors[0].cells[0];
        assert_eq!(cell.window_count, 1);
        assert!(cell.full_screen);
        assert!(cell.focused);
    }

    #[test]
    fn monocle_container_marks_full_screen() {
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [{
                        "containers": { "elements": [], "focused": 0 },
                        "floating_windows": { "elements": [], "focused": 0 },
                        "monocle_container": { "id": "x" }
                    }],
                    "focused": 0
                }
            }], "focused": 0 }
        }));
        let cell = w.monitors[0].cells[0];
        assert_eq!(cell.window_count, 1);
        assert!(cell.full_screen);
    }

    #[test]
    fn floating_only_workspace_is_non_empty() {
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [{
                        "containers": { "elements": [], "focused": 0 },
                        "floating_windows": { "elements": [{}], "focused": 0 }
                    }],
                    "focused": 0
                }
            }], "focused": 0 }
        }));
        let cell = w.monitors[0].cells[0];
        assert_eq!(cell.window_count, 1);
        assert!(!cell.full_screen);
    }

    #[test]
    fn focused_index_is_per_monitor() {
        let w = parse(json!({
            "monitors": { "elements": [
                {
                    "device_id": "DEV-A",
                    "workspaces": { "elements": [{}, {}, {}], "focused": 2 }
                },
                {
                    "device_id": "DEV-B",
                    "workspaces": { "elements": [{}, {}], "focused": 0 }
                }
            ], "focused": 1 }
        }));
        assert_eq!(w.monitors.len(), 2);
        assert!(!w.monitors[0].cells[0].focused);
        assert!(!w.monitors[0].cells[1].focused);
        assert!(w.monitors[0].cells[2].focused);
        assert!(w.monitors[1].cells[0].focused);
        assert!(!w.monitors[1].cells[1].focused);
    }

    #[test]
    fn active_flag_matches_outer_focused_index() {
        let w = parse(json!({
            "monitors": { "elements": [
                { "device_id": "DEV-A", "workspaces": { "elements": [], "focused": 0 } },
                { "device_id": "DEV-B", "workspaces": { "elements": [], "focused": 0 } },
                { "device_id": "DEV-C", "workspaces": { "elements": [], "focused": 0 } }
            ], "focused": 2 }
        }));
        assert!(!w.monitors[0].active);
        assert!(!w.monitors[1].active);
        assert!(w.monitors[2].active);
    }

    #[test]
    fn active_flag_set_for_single_monitor_too() {
        // Single-monitor case: the flag is still set; the tray manager is
        // responsible for not drawing the border in that case.
        let w = parse(json!({
            "monitors": { "elements": [
                { "device_id": "ONLY", "workspaces": { "elements": [], "focused": 0 } }
            ], "focused": 0 }
        }));
        assert_eq!(w.monitors.len(), 1);
        assert!(w.monitors[0].active);
    }

    #[test]
    fn extra_workspaces_beyond_nine_are_ignored() {
        let mut elements: Vec<serde_json::Value> = Vec::new();
        for _ in 0..12 {
            elements.push(json!({
                "containers": { "elements": [{}], "focused": 0 }
            }));
        }
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": { "elements": elements, "focused": 0 }
            }], "focused": 0 }
        }));
        for i in 0..9 {
            assert_eq!(w.monitors[0].cells[i].window_count, 1);
        }
    }

    #[test]
    fn missing_workspaces_pad_with_empty_cells() {
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [
                        { "containers": { "elements": [{}], "focused": 0 } },
                        { "containers": { "elements": [{}], "focused": 0 } }
                    ],
                    "focused": 0
                }
            }], "focused": 0 }
        }));
        let m = &w.monitors[0];
        assert_eq!(m.cells[0].window_count, 1);
        assert_eq!(m.cells[1].window_count, 1);
        for i in 2..9 {
            assert_eq!(m.cells[i], CellState::EMPTY);
        }
    }

    #[test]
    fn id_falls_back_when_device_id_missing() {
        let w = parse(json!({
            "monitors": { "elements": [
                { "device": "HPN3535", "workspaces": { "elements": [], "focused": 0 } },
                { "name": "DISPLAY1",  "workspaces": { "elements": [], "focused": 0 } },
                { "workspaces": { "elements": [], "focused": 0 } }
            ], "focused": 0 }
        }));
        assert_eq!(w.monitors[0].id, "device:HPN3535");
        assert_eq!(w.monitors[1].id, "name:DISPLAY1");
        assert_eq!(w.monitors[2].id, "__monitor_2__");
    }

    #[test]
    fn floating_windows_accepts_plain_array() {
        // Belt-and-braces test for the older komorebi shape.
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [{
                        "containers": { "elements": [], "focused": 0 },
                        "floating_windows": [{}]
                    }],
                    "focused": 0
                }
            }], "focused": 0 }
        }));
        assert_eq!(w.monitors[0].cells[0].window_count, 1);
    }

    #[test]
    fn window_count_adds_containers_floating_and_full_screen_sources() {
        let w = parse(json!({
            "monitors": { "elements": [{
                "device_id": "DEV-1",
                "workspaces": {
                    "elements": [{
                        "containers": { "elements": [{}, {}], "focused": 0 },
                        "floating_windows": { "elements": [{}, {}], "focused": 0 },
                        "monocle_container": { "id": "x" },
                        "maximized_window": { "hwnd": 1 }
                    }],
                    "focused": 0
                }
            }], "focused": 0 }
        }));

        let cell = w.monitors[0].cells[0];
        assert_eq!(cell.window_count, 6);
        assert!(cell.full_screen);
    }
}
