//! Integration tests for komorebi-state → `WorldState` projection.
//!
//! Each test loads a committed JSON fixture from
//! `tests/fixtures/komorebi/` and asserts the cells / monitors / id fields
//! of the resulting [`WorldState`].

use std::path::PathBuf;

use komorebi_tray_grid::komorebi::{state::WorldState, types};
use komorebi_tray_grid::render::CellState;

fn load(name: &str) -> WorldState {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("komorebi")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let parsed: types::State = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    WorldState::from(&parsed)
}

#[test]
fn single_monitor_focused_workspace_is_blue_and_others_follow_spec() {
    let w = load("single_monitor.json");
    assert_eq!(w.monitors.len(), 1);

    let m = &w.monitors[0];
    assert_eq!(m.id, "Generic_Monitor-1&abcdef&0&UID256");
    assert_eq!(m.label, "DISPLAY1");
    // Single monitor is always "active" per the projection; the tray
    // manager is the one that skips the highlight border when there's
    // only one monitor.
    assert!(m.active);

    // Cell 0: non-empty, not focused (focused is ws 1).
    assert!(m.cells[0].non_empty);
    assert!(!m.cells[0].focused);
    assert!(!m.cells[0].full_screen);

    // Cell 1: focused + non-empty.
    assert!(m.cells[1].focused);
    assert!(m.cells[1].non_empty);
    assert!(!m.cells[1].full_screen);

    // Cell 2: empty workspace.
    assert_eq!(m.cells[2], CellState::EMPTY);

    // Cells 3..8: not reported → empty.
    for i in 3..9 {
        assert_eq!(m.cells[i], CellState::EMPTY, "trailing cell {i}");
    }
}

#[test]
fn multi_monitor_each_icon_reflects_only_its_own_state() {
    let w = load("multi_monitor.json");
    assert_eq!(w.monitors.len(), 2);

    // Monitor A: focused ws is 0; ws0 non-empty; ws1 empty.
    let a = &w.monitors[0];
    assert_eq!(a.id, "MON-A");
    assert!(a.cells[0].focused);
    assert!(a.cells[0].non_empty);
    assert!(!a.cells[1].focused);
    assert!(!a.cells[1].non_empty);
    // monitors.focused is 1 in the fixture → only B is active.
    assert!(!a.active);

    // Monitor B: focused ws is 1; ws0 empty, ws1 + ws2 non-empty.
    let b = &w.monitors[1];
    assert_eq!(b.id, "MON-B");
    assert!(!b.cells[0].focused);
    assert!(!b.cells[0].non_empty);
    assert!(b.cells[1].focused);
    assert!(b.cells[1].non_empty);
    assert!(!b.cells[2].focused);
    assert!(b.cells[2].non_empty);
    assert!(b.active);

    // The two monitors' focus indices are independent.
    assert_ne!(
        a.cells.iter().position(|c| c.focused),
        b.cells.iter().position(|c| c.focused),
    );

    // Exactly one monitor is active.
    let active_count = w.monitors.iter().filter(|m| m.active).count();
    assert_eq!(active_count, 1, "exactly one monitor must be active");
}

#[test]
fn maximized_window_and_monocle_container_both_mark_full_screen() {
    let w = load("full_screen.json");
    let m = &w.monitors[0];

    // ws 0: only `maximized_window` set.
    assert!(m.cells[0].full_screen);
    assert!(m.cells[0].non_empty);
    assert!(m.cells[0].focused);

    // ws 1: only `monocle_container` set.
    assert!(m.cells[1].full_screen);
    assert!(m.cells[1].non_empty);
    assert!(!m.cells[1].focused);

    // ws 2: normal container, no full-screen marker.
    assert!(!m.cells[2].full_screen);
    assert!(m.cells[2].non_empty);
}

#[test]
fn empty_trailing_workspaces_render_as_empty_cells() {
    let w = load("empty_trailing.json");
    let m = &w.monitors[0];

    assert_eq!(m.id, "TRAIL-MON");
    assert!(m.cells[0].non_empty);
    assert!(m.cells[1].non_empty);

    // Only 2 workspaces reported → cells 2..8 must be the default `EMPTY`.
    for i in 2..9 {
        assert_eq!(m.cells[i], CellState::EMPTY, "cell {i} should be empty");
    }
}
