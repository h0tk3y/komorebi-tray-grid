//! Tray-icon image rasterizer.
//!
//! Pure-function CPU rasterization of the 3×3 status grid into an RGBA buffer.
//! The output is deterministic and dependency-free, which keeps the snapshot
//! tests trivial and the binary self-contained (no image assets at runtime).
//!
//! Layout (32 × 32, row-major, RGBA8):
//!
//! ```text
//!   ┌──┬──┬──┐    each cell  = 10 × 10 px
//!   │00│01│02│    gap        = 1  × 10 px (between cells)
//!   ├──┼──┼──┤    full image = 32 × 32 px
//!   │03│04│05│
//!   ├──┼──┼──┤    border (full-screen) = 2 px ring on top of the fill
//!   │06│07│08│
//!   └──┴──┴──┘
//! ```

/// Side length of the rendered tray icon, in pixels.
pub const ICON_SIZE: u32 = 32;

/// Side length of an individual cell, in pixels.
pub const CELL_SIZE: u32 = 10;

/// Gap between adjacent cells, in pixels. With three cells per row, this works
/// out to `3 * 10 + 2 * 1 = 32` pixels — i.e. fills the whole icon with no
/// outer margin.
pub const CELL_GAP: u32 = 1;

/// Thickness of the full-screen indicator border, in pixels.
pub const BORDER_THICKNESS: u32 = 2;

/// Bright blue fill for the focused workspace.
pub const COLOR_FOCUSED: [u8; 4] = [0x2E, 0x9B, 0xFF, 0xFF];

/// Gray fill for a non-empty (but not focused) workspace.
pub const COLOR_NON_EMPTY: [u8; 4] = [0x80, 0x80, 0x80, 0xFF];

/// Yellow border drawn on top of cells that contain a full-screen window
/// (maximized window or monocle container). Yellow is used so the indicator
/// stays high-contrast on top of both the bright-blue focused fill and the
/// gray non-empty fill.
pub const COLOR_BORDER_FS: [u8; 4] = [0xFF, 0xD5, 0x00, 0xFF];

/// Outer border drawn on top of the **whole icon** when the monitor that
/// owns the icon is the currently focused monitor. Uses the same blue as
/// the focused-cell fill so the "blue = active focus" semantic stays
/// coherent across cells and icons. Drawn on every icon, single- and
/// multi-monitor setups alike (in the single-monitor case the only icon
/// is by definition active and gets this color).
pub const COLOR_ACTIVE_MONITOR: [u8; 4] = COLOR_FOCUSED;

/// Outer border drawn on top of the **whole icon** for monitors that are
/// *not* currently focused. Uses the same gray as a non-empty cell so all
/// icons share the same visual footprint and the active highlight reads
/// as a state change rather than a size change.
pub const COLOR_INACTIVE_MONITOR: [u8; 4] = COLOR_NON_EMPTY;

/// Thickness of the per-monitor outer border (active or inactive), in pixels.
pub const MONITOR_BORDER: u32 = 1;

/// Fully transparent pixel used as the empty-cell background.
pub const COLOR_TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

const _: () = {
    // Compile-time sanity check that the geometry actually fills the icon.
    assert!(3 * CELL_SIZE + 2 * CELL_GAP == ICON_SIZE);
};

/// Per-cell visual state.
///
/// The three flags are independent and compose: a cell can be focused **and**
/// host a full-screen window, in which case the cell is filled blue and gets a
/// yellow border on top.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct CellState {
    /// The workspace at this cell is the focused workspace on its monitor.
    pub focused: bool,
    /// The workspace contains at least one container (i.e. window).
    pub non_empty: bool,
    /// The workspace contains a full-screen / maximized container.
    pub full_screen: bool,
}

impl CellState {
    pub const EMPTY: Self = Self {
        focused: false,
        non_empty: false,
        full_screen: false,
    };
}

/// Rasterize a 3×3 grid of cells into an `ICON_SIZE × ICON_SIZE` RGBA buffer.
///
/// The buffer is in row-major, top-left origin, RGBA8 order — directly
/// consumable by `tray_icon::Icon::from_rgba`. Returned length is always
/// `(ICON_SIZE * ICON_SIZE * 4)` bytes.
pub fn render_grid(cells: &[CellState; 9]) -> Vec<u8> {
    let mut pixels = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];

    for (i, cell) in cells.iter().enumerate() {
        let col = (i % 3) as u32;
        let row = (i / 3) as u32;

        let x0 = col * (CELL_SIZE + CELL_GAP);
        let y0 = row * (CELL_SIZE + CELL_GAP);
        let x1 = x0 + CELL_SIZE;
        let y1 = y0 + CELL_SIZE;

        // Fill (focused beats non-empty; empty stays transparent).
        let fill = if cell.focused {
            Some(COLOR_FOCUSED)
        } else if cell.non_empty {
            Some(COLOR_NON_EMPTY)
        } else {
            None
        };
        if let Some(color) = fill {
            fill_rect(&mut pixels, x0, y0, x1, y1, color);
        }

        // Border (composes with whatever fill is below).
        if cell.full_screen {
            draw_border(
                &mut pixels,
                x0,
                y0,
                x1,
                y1,
                BORDER_THICKNESS,
                COLOR_BORDER_FS,
            );
        }
    }

    pixels
}

#[inline]
fn put_pixel(pixels: &mut [u8], x: u32, y: u32, color: [u8; 4]) {
    let idx = ((y * ICON_SIZE + x) * 4) as usize;
    pixels[idx..idx + 4].copy_from_slice(&color);
}

fn fill_rect(pixels: &mut [u8], x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 4]) {
    for y in y0..y1 {
        for x in x0..x1 {
            put_pixel(pixels, x, y, color);
        }
    }
}

/// Overlay the per-monitor outer border on top of an already-rendered
/// icon buffer. Called from the tray manager for every icon, regardless
/// of how many monitors are present — see `tray.rs`. When `active` is
/// `true`, the focused (blue) color is used; otherwise, the non-empty
/// (gray) color is used.
///
/// The border is drawn as a `MONITOR_BORDER`-pixel ring on the outermost
/// rim of the icon; on a 32 × 32 icon this overlaps the outer rim of the
/// edge cells by at most one pixel, which is visually unnoticeable after
/// Windows scales the icon down to its tray size.
pub fn paint_monitor_border(pixels: &mut [u8], active: bool) {
    let color = if active {
        COLOR_ACTIVE_MONITOR
    } else {
        COLOR_INACTIVE_MONITOR
    };
    draw_border(pixels, 0, 0, ICON_SIZE, ICON_SIZE, MONITOR_BORDER, color);
}

fn draw_border(
    pixels: &mut [u8],
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    thickness: u32,
    color: [u8; 4],
) {
    for y in y0..y1 {
        for x in x0..x1 {
            let on_top = y < y0 + thickness;
            let on_bottom = y >= y1 - thickness;
            let on_left = x < x0 + thickness;
            let on_right = x >= x1 - thickness;
            if on_top || on_bottom || on_left || on_right {
                put_pixel(pixels, x, y, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel_at(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * ICON_SIZE + x) * 4) as usize;
        [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
    }

    #[test]
    fn output_buffer_has_expected_size() {
        let buf = render_grid(&[CellState::EMPTY; 9]);
        assert_eq!(buf.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }

    #[test]
    fn empty_grid_is_fully_transparent() {
        let buf = render_grid(&[CellState::EMPTY; 9]);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn focused_overrides_non_empty() {
        let mut cells = [CellState::EMPTY; 9];
        cells[0] = CellState {
            focused: true,
            non_empty: true,
            full_screen: false,
        };
        let buf = render_grid(&cells);
        // Pixel inside the first cell should be the focused color.
        assert_eq!(pixel_at(&buf, 1, 1), COLOR_FOCUSED);
    }

    #[test]
    fn full_screen_paints_border_on_top_of_fill() {
        let mut cells = [CellState::EMPTY; 9];
        cells[4] = CellState {
            focused: true,
            non_empty: false,
            full_screen: true,
        };
        let buf = render_grid(&cells);

        // Center cell starts at (11,11) and spans 10×10.
        // Top-left corner is inside the border ring (yellow).
        assert_eq!(pixel_at(&buf, 11, 11), COLOR_BORDER_FS);
        // Two pixels inside the border ring → fill color.
        assert_eq!(pixel_at(&buf, 13, 13), COLOR_FOCUSED);
    }

    #[test]
    fn empty_full_screen_cell_only_draws_border() {
        let mut cells = [CellState::EMPTY; 9];
        cells[8] = CellState {
            focused: false,
            non_empty: false,
            full_screen: true,
        };
        let buf = render_grid(&cells);

        // Cell 8 (bottom-right) starts at (22,22) and spans 10×10.
        // Inside border → yellow.
        assert_eq!(pixel_at(&buf, 22, 22), COLOR_BORDER_FS);
        // Two pixels inside the border → fully transparent (no fill).
        assert_eq!(pixel_at(&buf, 24, 24), COLOR_TRANSPARENT);
        // Sanity check: the configured border color is yellow, not purple.
        assert_eq!(COLOR_BORDER_FS, [0xFF, 0xD5, 0x00, 0xFF]);
    }

    #[test]
    fn active_monitor_border_paints_outer_ring_only() {
        let mut buf = render_grid(&[CellState::EMPTY; 9]);
        paint_monitor_border(&mut buf, true);

        // Outermost rim is the active-monitor color.
        for k in 0..ICON_SIZE {
            assert_eq!(pixel_at(&buf, k, 0), COLOR_ACTIVE_MONITOR);
            assert_eq!(pixel_at(&buf, k, ICON_SIZE - 1), COLOR_ACTIVE_MONITOR);
            assert_eq!(pixel_at(&buf, 0, k), COLOR_ACTIVE_MONITOR);
            assert_eq!(pixel_at(&buf, ICON_SIZE - 1, k), COLOR_ACTIVE_MONITOR);
        }
        // One pixel inside the rim stays transparent (still empty grid).
        assert_eq!(pixel_at(&buf, 1, 1), COLOR_TRANSPARENT);
        assert_eq!(pixel_at(&buf, ICON_SIZE - 2, ICON_SIZE - 2), COLOR_TRANSPARENT);
    }

    #[test]
    fn inactive_monitor_border_uses_gray() {
        let mut buf = render_grid(&[CellState::EMPTY; 9]);
        paint_monitor_border(&mut buf, false);

        // Outermost rim is the non-empty (gray) color.
        for k in 0..ICON_SIZE {
            assert_eq!(pixel_at(&buf, k, 0), COLOR_INACTIVE_MONITOR);
            assert_eq!(pixel_at(&buf, k, ICON_SIZE - 1), COLOR_INACTIVE_MONITOR);
            assert_eq!(pixel_at(&buf, 0, k), COLOR_INACTIVE_MONITOR);
            assert_eq!(pixel_at(&buf, ICON_SIZE - 1, k), COLOR_INACTIVE_MONITOR);
        }
        // Sanity check: the configured inactive color matches the gray
        // non-empty cell fill, not the focused blue.
        assert_eq!(COLOR_INACTIVE_MONITOR, COLOR_NON_EMPTY);
        assert_ne!(COLOR_INACTIVE_MONITOR, COLOR_ACTIVE_MONITOR);
        // One pixel inside the rim stays transparent (still empty grid).
        assert_eq!(pixel_at(&buf, 1, 1), COLOR_TRANSPARENT);
    }

    #[test]
    fn gap_pixels_stay_transparent() {
        let cells = [CellState {
            focused: true,
            non_empty: true,
            full_screen: true,
        }; 9];
        let buf = render_grid(&cells);

        // Gap column between cell 0 and cell 1 is x = 10.
        for y in 0..ICON_SIZE {
            assert_eq!(
                pixel_at(&buf, 10, y),
                COLOR_TRANSPARENT,
                "gap column should be transparent at y={y}",
            );
        }
        // Gap row between cell 0 and cell 3 is y = 10.
        for x in 0..ICON_SIZE {
            assert_eq!(
                pixel_at(&buf, x, 10),
                COLOR_TRANSPARENT,
                "gap row should be transparent at x={x}",
            );
        }
    }
}
