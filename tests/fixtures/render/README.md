# Render snapshots

PNG snapshots of the tray-icon renderer output (`render::render_grid`).
Each test in `tests/render.rs` compares the rendered RGBA bytes against the
corresponding file here.

To regenerate after an intentional visual change:

```powershell
$env:KOMOREBI_TRAY_GRID_REGEN = "1"
cargo test --test render --locked
Remove-Item Env:\KOMOREBI_TRAY_GRID_REGEN
```

Then commit the updated `*.png` files.
