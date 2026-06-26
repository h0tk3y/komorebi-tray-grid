// Library entry point for `komorebi-tray-grid`.
//
// The binary (`src/main.rs`) is a thin wrapper around this crate; pulling the
// actual logic into a library makes the pure pieces (icon rendering, komorebi
// state mapping) reachable from `tests/`.

pub mod app;
pub mod autostart;
pub mod config;
pub mod event;
pub mod komorebi;
pub mod render;
pub mod single_instance;
pub mod tray;
