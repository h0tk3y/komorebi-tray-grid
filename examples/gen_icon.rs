//! Regenerate `assets/app.ico` from the in-process renderer.
//!
//! This is run on demand (e.g. when the grid style changes), not on every
//! build, so the produced `.ico` is committed to the repo and consumed
//! both by the Windows resource embed (`app.rc`) and by `cargo packager`.
//!
//! Usage:
//!
//! ```text
//! cargo run --example gen_icon
//! ```
//!
//! Output: `assets/app.ico` (multi-resolution ICO: 16, 32, 48, 64, 128, 256).
//!
//! Design choice: we render the grid once at 32×32 RGBA — the size the app
//! actually paints — with the active-monitor (focused) blue outer border,
//! then resample it with nearest-neighbor to the other resolutions so the
//! crisp pixel-grid look is preserved at every size. The resulting icon
//! visually matches a single tray cell of a freshly launched, focused app.

use std::fs;
use std::path::PathBuf;

use komorebi_tray_grid::render::{
    paint_monitor_border, render_grid, CellState, COLOR_FOCUSED, ICON_SIZE,
};

/// Sizes the produced ICO will contain. Windows picks the closest one at
/// runtime; including a 256 entry also gives a sharp display in Explorer.
const SIZES: &[u32] = &[16, 32, 48, 64, 128, 256];

fn main() -> anyhow::Result<()> {
    // A representative "fresh launch on the focused monitor" icon: workspace 0
    // focused, everything else empty, with the active-monitor blue ring.
    let mut cells = [CellState::EMPTY; 9];
    cells[0] = CellState {
        focused: true,
        non_empty: false,
        full_screen: false,
    };

    let mut base = render_grid(&cells);
    paint_monitor_border(&mut base, true);
    debug_assert_eq!(base.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    // Sanity: pixel (0,0) is the active-monitor ring color.
    debug_assert_eq!(&base[0..4], &COLOR_FOCUSED);

    let mut icon = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in SIZES {
        let scaled = nearest_neighbor_rgba(&base, ICON_SIZE, ICON_SIZE, size, size);
        let image = ico::IconImage::from_rgba_data(size, size, scaled);
        icon.add_entry(ico::IconDirEntry::encode(&image)?);
    }

    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join("app.ico");
    let file = fs::File::create(&out_path)?;
    icon.write(file)?;

    println!("wrote {} ({} sizes)", out_path.display(), SIZES.len());

    let png_path = out_dir.join("../docs/icon.png");
    image::save_buffer(
        &png_path,
        &base,
        ICON_SIZE,
        ICON_SIZE,
        image::ExtendedColorType::Rgba8,
    )?;
    println!("wrote {}", png_path.display());

    Ok(())
}

/// Nearest-neighbor RGBA resampling. Tiny, dependency-free, and produces
/// sharp pixel-art that matches the rendered grid at every output size.
fn nearest_neighbor_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut dst = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        let sy = (y * sh) / dh;
        for x in 0..dw {
            let sx = (x * sw) / dw;
            let s = ((sy * sw + sx) * 4) as usize;
            let d = ((y * dw + x) * 4) as usize;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    dst
}
