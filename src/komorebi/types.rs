//! Serde structs that mirror the subset of `komorebic state` JSON we actually
//! consume.
//!
//! The goal is to be resilient to komorebi schema evolution:
//!
//! - All structs use `#[serde(default)]` so unknown / missing fields don't
//!   cause deserialization to fail.
//! - "Container-shaped" fields (`containers`, `floating_windows`, …) are
//!   decoded as `serde_json::Value` so we only inspect their length, not
//!   their per-window contents.
//! - The "Ring" pattern (`{ "elements": [...], "focused": N }`) komorebi
//!   uses throughout is represented by [`Ring<T>`].

use serde::Deserialize;
use serde_json::Value as JsonValue;

/// komorebi's pervasive `Ring<T>` pattern: an ordered list of items plus the
/// index of the focused one. Missing fields default to an empty ring focused
/// at index 0.
#[derive(Debug, Deserialize, Clone)]
pub struct Ring<T> {
    #[serde(default = "Vec::new")]
    pub elements: Vec<T>,
    #[serde(default)]
    pub focused: usize,
}

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            focused: 0,
        }
    }
}

impl<T> Ring<T> {
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
    pub fn len(&self) -> usize {
        self.elements.len()
    }
}

/// Top-level shape of `komorebic state`.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct State {
    pub monitors: Ring<Monitor>,
}

/// Rectangle dimensions as reported by komorebi (`left`/`top` are the
/// top-left origin in virtual-desktop coordinates). Only `left` is currently
/// used, to order monitors from left to right.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// One physical (or virtual) monitor as reported by komorebi.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Monitor {
    /// Stable monitor identity (komorebi calls it `device_id`). Used as the
    /// per-monitor tray-icon key.
    pub device_id: String,
    /// Friendly device name (e.g. `"HPN3535"`); used as a fallback key.
    pub device: String,
    /// Display name (e.g. `"DISPLAY1"`); used as a last-resort fallback.
    pub name: String,
    /// Physical bounds of the monitor; its `left` coordinate lets us order
    /// monitors from left to right.
    pub size: Rect,
    pub workspaces: Ring<Workspace>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Window {
    pub title: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Container {
    pub windows: Ring<Window>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum WindowCollection {
    Ring(Ring<Window>),
    Array(Vec<Window>),
    Other(JsonValue),
}

impl Default for WindowCollection {
    fn default() -> Self {
        Self::Array(Vec::new())
    }
}

impl WindowCollection {
    pub fn len(&self) -> usize {
        match self {
            Self::Ring(ring) => ring.len(),
            Self::Array(windows) => windows.len(),
            Self::Other(value) => json_collection_len(value),
        }
    }

    pub fn titles(&self) -> Vec<String> {
        match self {
            Self::Ring(ring) => ring.elements.iter().filter_map(window_title).collect(),
            Self::Array(windows) => windows.iter().filter_map(window_title).collect(),
            Self::Other(_) => Vec::new(),
        }
    }
}

fn window_title(window: &Window) -> Option<String> {
    let title = window.title.trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

fn json_collection_len(v: &JsonValue) -> usize {
    match v {
        JsonValue::Array(arr) => arr.len(),
        JsonValue::Object(obj) => match obj.get("elements") {
            Some(JsonValue::Array(arr)) => arr.len(),
            _ => 0,
        },
        _ => 0,
    }
}

/// One workspace on a monitor. Only the fields needed to derive the three
/// per-cell flags (`focused`, `non_empty`, `full_screen`) are modeled.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Workspace {
    /// Tiled containers (each item itself contains windows). Non-empty ⇒
    /// the workspace has windows.
    pub containers: Ring<Container>,
    /// Floating windows. Modeled as a generic JSON value because older
    /// komorebi versions exposed this as a plain array while newer ones use
    /// the `Ring` shape; see [`json_collection_non_empty`].
    pub floating_windows: WindowCollection,
    /// Set when the workspace is in monocle (single-container fullscreen)
    /// mode.
    pub monocle_container: Option<Container>,
    /// Set when one window in the workspace is currently maximized.
    pub maximized_window: Option<Window>,
}

/// `true` if `v` represents a non-empty collection, whether it's stored as
/// a plain JSON array or as the `Ring`-shaped `{ "elements": [...] }`.
pub fn json_collection_non_empty(v: &JsonValue) -> bool {
    match v {
        JsonValue::Array(arr) => !arr.is_empty(),
        JsonValue::Object(obj) => match obj.get("elements") {
            Some(JsonValue::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

pub fn container_titles(container: &Container) -> Vec<String> {
    container
        .windows
        .elements
        .iter()
        .filter_map(window_title)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ring_default_is_empty() {
        let r: Ring<i32> = Default::default();
        assert!(r.is_empty());
        assert_eq!(r.focused, 0);
    }

    #[test]
    fn ring_deserializes_from_komorebi_shape() {
        let v: Ring<i32> = serde_json::from_value(json!({
            "elements": [10, 20, 30],
            "focused": 1
        }))
        .unwrap();
        assert_eq!(v.elements, vec![10, 20, 30]);
        assert_eq!(v.focused, 1);
    }

    #[test]
    fn ring_tolerates_missing_focused() {
        let v: Ring<i32> = serde_json::from_value(json!({ "elements": [1, 2] })).unwrap();
        assert_eq!(v.focused, 0);
    }

    #[test]
    fn state_tolerates_unknown_fields() {
        // Real komorebic state output has many fields we don't model;
        // serde(default) must not reject them.
        let raw = json!({
            "monitors": {
                "elements": [],
                "focused": 0
            },
            "is_paused": false,
            "resize_delta": 50,
            "something_brand_new": [1, 2, 3]
        });
        let s: State = serde_json::from_value(raw).unwrap();
        assert!(s.monitors.elements.is_empty());
    }

    #[test]
    fn floating_windows_accepts_both_shapes() {
        let ring = json!({ "elements": [{"title":"A"}], "focused": 0 });
        let arr = json!([{"title":"B"}]);
        let empty_ring = json!({ "elements": [], "focused": 0 });
        let empty_arr = json!([]);
        assert!(json_collection_non_empty(&ring));
        assert!(json_collection_non_empty(&arr));
        assert!(!json_collection_non_empty(&empty_ring));
        assert!(!json_collection_non_empty(&empty_arr));
        assert!(!json_collection_non_empty(&JsonValue::Null));

        let parsed_ring: WindowCollection = serde_json::from_value(ring).unwrap();
        assert_eq!(parsed_ring.len(), 1);
        assert_eq!(parsed_ring.titles(), vec!["A".to_string()]);

        let parsed_arr: WindowCollection = serde_json::from_value(arr).unwrap();
        assert_eq!(parsed_arr.len(), 1);
        assert_eq!(parsed_arr.titles(), vec!["B".to_string()]);
    }
}
