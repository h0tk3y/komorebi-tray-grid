//! Snapshot tests for the tray-icon renderer.
//!
//! Each test renders a representative cell combination and compares the raw
//! RGBA bytes against a PNG fixture committed under
//! `tests/fixtures/render/`. To regenerate the fixtures after an intentional
//! visual change, set the `KOMOREBI_TRAY_GRID_REGEN` environment variable when
//! running the tests:
//!
//! ```powershell
//! $env:KOMOREBI_TRAY_GRID_REGEN = "1"; cargo test --test render
//! Remove-Item Env:\KOMOREBI_TRAY_GRID_REGEN
//! ```

use std::path::PathBuf;

use komorebi_tray_grid::render::{render_grid, CellState, ICON_SIZE};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("render")
        .join(name)
}

fn cells_from(states: [(bool, bool, bool); 9]) -> [CellState; 9] {
    let mut out = [CellState::default(); 9];
    for (i, &(focused, non_empty, full_screen)) in states.iter().enumerate() {
        out[i] = CellState {
            focused,
            non_empty,
            full_screen,
        };
    }
    out
}

fn assert_snapshot(name: &str, rgba: &[u8]) {
    assert_eq!(
        rgba.len(),
        (ICON_SIZE * ICON_SIZE * 4) as usize,
        "render_grid must produce ICON_SIZE × ICON_SIZE × 4 bytes",
    );

    let path = fixture_path(name);
    let regen = std::env::var_os("KOMOREBI_TRAY_GRID_REGEN").is_some();

    if regen || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap())
            .expect("create fixtures directory");
        let img = image::RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, rgba.to_vec())
            .expect("RGBA buffer must match ICON_SIZE × ICON_SIZE");
        img.save(&path).expect("write fixture PNG");
        if !regen {
            // First-time generation: emit a hint so CI doesn't silently bless
            // a missing fixture.
            eprintln!(
                "snapshot fixture created at {} (commit it; rerun without REGEN to verify)",
                path.display()
            );
        }
        return;
    }

    let img = image::open(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
        .into_rgba8();
    assert_eq!(
        img.dimensions(),
        (ICON_SIZE, ICON_SIZE),
        "fixture {} has wrong dimensions",
        path.display(),
    );
    assert_eq!(
        img.as_raw().as_slice(),
        rgba,
        "snapshot mismatch for {}; \
         if the change is intentional, regenerate with \
         `$env:KOMOREBI_TRAY_GRID_REGEN = \"1\"; cargo test --test render`",
        name,
    );
}

#[test]
fn snapshot_all_empty() {
    let grid = render_grid(&[CellState::EMPTY; 9]);
    assert_snapshot("all_empty.png", &grid);
}

#[test]
fn snapshot_single_focused() {
    let mut states = [(false, false, false); 9];
    states[4] = (true, false, false); // center cell focused
    assert_snapshot("single_focused.png", &render_grid(&cells_from(states)));
}

#[test]
fn snapshot_single_non_empty() {
    let mut states = [(false, false, false); 9];
    states[0] = (false, true, false); // top-left has windows
    assert_snapshot("single_non_empty.png", &render_grid(&cells_from(states)));
}

#[test]
fn snapshot_focused_full_screen() {
    let mut states = [(false, false, false); 9];
    states[4] = (true, true, true); // center: focused + maximized
    assert_snapshot(
        "focused_full_screen.png",
        &render_grid(&cells_from(states)),
    );
}

#[test]
fn snapshot_non_empty_full_screen() {
    let mut states = [(false, false, false); 9];
    states[8] = (false, true, true); // bottom-right: maximized, not focused
    assert_snapshot(
        "non_empty_full_screen.png",
        &render_grid(&cells_from(states)),
    );
}

#[test]
fn snapshot_mixed_grid() {
    // A representative mix exercising every state combination at once.
    let states = [
        (false, true, false),   // 0: gray
        (true, true, false),    // 1: blue (focused + non-empty)
        (false, false, false),  // 2: empty
        (false, true, true),    // 3: gray + yellow border
        (false, false, false),  // 4: empty
        (true, true, true),     // 5: blue + yellow border
        (false, false, true),   // 6: only border (empty workspace, weird but allowed)
        (false, true, false),   // 7: gray
        (false, false, false),  // 8: empty
    ];
    assert_snapshot("mixed_grid.png", &render_grid(&cells_from(states)));
}
