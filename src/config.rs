//! App configuration loading.

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::render::Theme;

#[derive(Debug, Deserialize, Default)]
struct AppConfig {
    #[serde(default)]
    colors: ColorConfig,
}

#[derive(Debug, Deserialize)]
struct ColorConfig {
    #[serde(default = "default_focused")]
    focused: String,
    #[serde(default = "default_non_empty_1")]
    non_empty_1: String,
    #[serde(default = "default_non_empty_2")]
    non_empty_2: String,
    #[serde(default = "default_non_empty_3_plus")]
    non_empty_3_plus: String,
    #[serde(default = "default_full_screen_border")]
    full_screen_border: String,
    #[serde(default = "default_active_monitor_border")]
    active_monitor_border: String,
    #[serde(default = "default_inactive_monitor_border")]
    inactive_monitor_border: String,
    #[serde(default = "default_empty")]
    empty: String,
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            focused: default_focused(),
            non_empty_1: default_non_empty_1(),
            non_empty_2: default_non_empty_2(),
            non_empty_3_plus: default_non_empty_3_plus(),
            full_screen_border: default_full_screen_border(),
            active_monitor_border: default_active_monitor_border(),
            inactive_monitor_border: default_inactive_monitor_border(),
            empty: default_empty(),
        }
    }
}

pub fn load_theme() -> Theme {
    match try_load_theme() {
        Ok(theme) => theme,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load config; using default colors");
            Theme::default()
        }
    }
}

fn try_load_theme() -> Result<Theme> {
    let Some(path) = default_config_path() else {
        return Ok(Theme::default());
    };

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Theme::default()),
        Err(e) => {
            return Err(e).with_context(|| format!("read config file {}", path.display()));
        }
    };

    let config: AppConfig = serde_json::from_str(&text)
        .with_context(|| format!("parse config file {} as JSON", path.display()))?;

    Ok(Theme {
        focused: parse_hex_color(&config.colors.focused)
            .context("parse colors.focused as hex color")?,
        non_empty_1: parse_hex_color(&config.colors.non_empty_1)
            .context("parse colors.non_empty_1 as hex color")?,
        non_empty_2: parse_hex_color(&config.colors.non_empty_2)
            .context("parse colors.non_empty_2 as hex color")?,
        non_empty_3_plus: parse_hex_color(&config.colors.non_empty_3_plus)
            .context("parse colors.non_empty_3_plus as hex color")?,
        full_screen_border: parse_hex_color(&config.colors.full_screen_border)
            .context("parse colors.full_screen_border as hex color")?,
        active_monitor_border: parse_hex_color(&config.colors.active_monitor_border)
            .context("parse colors.active_monitor_border as hex color")?,
        inactive_monitor_border: parse_hex_color(&config.colors.inactive_monitor_border)
            .context("parse colors.inactive_monitor_border as hex color")?,
        empty: parse_hex_color(&config.colors.empty).context("parse colors.empty as hex color")?,
    })
}

fn default_config_path() -> Option<PathBuf> {
    let app_data = env::var_os("APPDATA")?;
    Some(PathBuf::from(app_data).join("komorebi-tray-grid").join("config.json"))
}

fn parse_hex_color(value: &str) -> Result<[u8; 4]> {
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| anyhow!("must start with '#'"))?;

    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).context("invalid red channel")?;
            let g = u8::from_str_radix(&hex[2..4], 16).context("invalid green channel")?;
            let b = u8::from_str_radix(&hex[4..6], 16).context("invalid blue channel")?;
            Ok([r, g, b, 0xFF])
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).context("invalid red channel")?;
            let g = u8::from_str_radix(&hex[2..4], 16).context("invalid green channel")?;
            let b = u8::from_str_radix(&hex[4..6], 16).context("invalid blue channel")?;
            let a = u8::from_str_radix(&hex[6..8], 16).context("invalid alpha channel")?;
            Ok([r, g, b, a])
        }
        _ => Err(anyhow!("must be #RRGGBB or #RRGGBBAA")),
    }
}

fn default_focused() -> String {
    "#2E9BFFFF".to_string()
}

fn default_non_empty_1() -> String {
    "#6B6B6BFF".to_string()
}

fn default_non_empty_2() -> String {
    "#8C8C8CFF".to_string()
}

fn default_non_empty_3_plus() -> String {
    "#B0B0B0FF".to_string()
}

fn default_full_screen_border() -> String {
    "#FFD500FF".to_string()
}

fn default_active_monitor_border() -> String {
    "#2E9BFFFF".to_string()
}

fn default_inactive_monitor_border() -> String {
    "#808080FF".to_string()
}

fn default_empty() -> String {
    "#00000000".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rgb_assumes_opaque_alpha() {
        assert_eq!(parse_hex_color("#112233").unwrap(), [0x11, 0x22, 0x33, 0xFF]);
    }

    #[test]
    fn parse_rgba_uses_given_alpha() {
        assert_eq!(parse_hex_color("#11223344").unwrap(), [0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn parse_requires_hash_prefix() {
        assert!(parse_hex_color("112233").is_err());
    }

    #[test]
    fn parse_rejects_invalid_lengths() {
        assert!(parse_hex_color("#1122").is_err());
        assert!(parse_hex_color("#1122334455").is_err());
    }
}