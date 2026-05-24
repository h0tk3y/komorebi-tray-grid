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
    pub workspaces: Ring<Workspace>,
}

/// One workspace on a monitor. Only the fields needed to derive the three
/// per-cell flags (`focused`, `non_empty`, `full_screen`) are modeled.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Workspace {
    /// Tiled containers (each item itself contains windows). Non-empty ⇒
    /// the workspace has windows.
    pub containers: Ring<JsonValue>,
    /// Floating windows. Modeled as a generic JSON value because older
    /// komorebi versions exposed this as a plain array while newer ones use
    /// the `Ring` shape; see [`json_collection_non_empty`].
    pub floating_windows: JsonValue,
    /// Set when the workspace is in monocle (single-container fullscreen)
    /// mode.
    pub monocle_container: Option<JsonValue>,
    /// Set when one window in the workspace is currently maximized.
    pub maximized_window: Option<JsonValue>,
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
        let v: Ring<i32> =
            serde_json::from_value(json!({ "elements": [1, 2] })).unwrap();
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
        let ring = json!({ "elements": [{}], "focused": 0 });
        let arr = json!([{}]);
        let empty_ring = json!({ "elements": [], "focused": 0 });
        let empty_arr = json!([]);
        assert!(json_collection_non_empty(&ring));
        assert!(json_collection_non_empty(&arr));
        assert!(!json_collection_non_empty(&empty_ring));
        assert!(!json_collection_non_empty(&empty_arr));
        assert!(!json_collection_non_empty(&JsonValue::Null));
    }
}
