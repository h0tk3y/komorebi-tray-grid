//! Integration with the [komorebi](https://github.com/LGUG2Z/komorebi) tiling
//! window manager.
//!
//! - [`types`] mirrors the subset of `komorebic state` JSON we consume.
//! - [`state`] derives a UI-friendly [`state::WorldState`] (per-monitor 3×3
//!   cells) from the raw types.
//! - [`pipe`] runs an async worker that subscribes to komorebi via a named
//!   pipe and emits debounced [`state::WorldState`] snapshots.

pub mod client;
pub mod pipe;
pub mod state;
pub mod types;
