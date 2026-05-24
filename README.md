# komorebi-tray-grid

A tiny Windows system-tray indicator for the [komorebi](https://github.com/LGUG2Z/komorebi) tiling
window manager. Each tray icon is a 3×3 grid that mirrors the state of the nine workspaces on a
single monitor; when multiple monitors are connected, one tray icon is created per monitor.

Per-cell visuals:

| Cell state                                  | Appearance                                   |
| ------------------------------------------- | -------------------------------------------- |
| empty workspace                             | transparent (no fill)                        |
| non-empty workspace                         | gray fill                                    |
| focused workspace                           | bright blue fill                             |
| workspace contains a full-screen container  | yellow border (composes with the fill above) |

Right-clicking any tray icon opens the same menu, with:

- **Enable autostart** — toggle a per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  entry pointing at the running executable, so the app launches on logon.
- **Quit** — terminate the app and remove all tray icons.

See [`spec.md`](spec.md) for the original spec and [`plan.md`](plan.md) for the design plan.

## Status

⚠️ Work in progress. The repository is currently at the Step 1 scaffold (buildable empty binary).
Subsequent delivery steps add the renderer, the komorebi event worker, the tray UI, the
autostart toggle, and the single-instance guard.

## Build

Prerequisites:

- Windows 10 or 11
- The pinned Rust toolchain from [`rust-toolchain.toml`](rust-toolchain.toml) (currently `1.90.0`,
  `x86_64-pc-windows-msvc` target). With [`rustup`](https://rustup.rs) installed, the toolchain
  is picked up automatically on the first `cargo` invocation in this directory.

```powershell
# Debug build
cargo build

# Release build (this is what CI ships)
cargo build --release --locked

# Run the tests (renderer + komorebi state mapping)
cargo test --locked
```

The release binary lands at `target\release\komorebi-tray-grid.exe`.

### App icon

The Windows resource compiler embeds `assets\app.ico` as the exe's Explorer / Alt-Tab
icon, and the same file is used by the NSIS installer (see below). It is generated
on demand from the in-process renderer so it always matches the tray's visual style:

```powershell
# Regenerate assets\app.ico (multi-resolution: 16/32/48/64/128/256).
cargo run --example gen_icon
```

The file is committed to the repository, so a fresh checkout builds without
running this step first; build still works (without an icon) even if you
delete `assets\app.ico`.

### Windows installer

A signed-able NSIS installer can be built with
[`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager):

```powershell
# One-time setup
cargo install cargo-packager --locked

# Build the installer (.exe). Runs `cargo build --release --locked` internally.
cargo packager --release
```

The resulting `komorebi-tray-grid_<version>_x64-setup.exe` lands in
`target\packager\`. The installer is **per-user only** (`%LOCALAPPDATA%\Programs\…`,
no UAC elevation): the app communicates with komorebi over a named pipe at the
user's integrity level, and an elevated install would auto-launch the app with
a High-IL token that komorebi (Medium-IL) cannot write events into.

Installer settings live under `[package.metadata.packager]` in
[`Cargo.toml`](Cargo.toml); to additionally build a `.msi`, add
`"wix"` to the `formats` array.

## Run

`komorebi` and `komorebic.exe` must be installed and reachable on `PATH`. Start komorebi as usual,
then launch:

```powershell
.\target\release\komorebi-tray-grid.exe
```

The app:

1. Acquires a `Global\komorebi-tray-grid` named mutex so only one instance can run.
2. Creates a uniquely named Windows named pipe and registers it with
   `komorebic subscribe-pipe <name>`.
3. Calls `komorebic state` to seed the initial state.
4. Adds one tray icon per monitor reported by komorebi and updates it on every event,
   coalescing bursts (~50 ms debounce).
5. If the pipe breaks, re-subscribes with exponential backoff and re-queries `komorebic state`.

Logs go to stderr; the log level can be tuned with `KOMOREBI_TRAY_LOG`, e.g.
`$env:KOMOREBI_TRAY_LOG = "debug"`.

## Releases

Tagged pushes (`v*`) trigger
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds on
`windows-latest`, runs tests, and uploads both a zipped portable `.exe` and the
NSIS `*-setup.exe` installer as release assets.
