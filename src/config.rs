//! App configuration loading.

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::render::Theme;
use crate::windows_theme::ColorScheme;

#[derive(Debug, Deserialize, Default)]
struct AppConfig {
    #[serde(default)]
    colors: ColorsConfig,
    #[serde(default)]
    menu: MenuConfig,
}

#[derive(Debug, Deserialize, Default)]
struct MenuConfig {
    pub show_hotkey: Option<String>,
    #[serde(default = "default_max_title_length")]
    pub max_title_length: usize,
    #[serde(default = "default_max_combined_title_length")]
    pub max_combined_title_length: usize,
}

#[derive(Debug, Deserialize)]
struct ColorsConfig {
    #[serde(default = "default_dark_colors")]
    dark: ColorConfig,
    #[serde(default = "default_light_colors")]
    light: ColorConfig,
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

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            dark: default_dark_colors(),
            light: default_light_colors(),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct ThemeSet {
    pub dark: Theme,
    pub light: Theme,
}

impl ThemeSet {
    pub fn for_scheme(&self, scheme: ColorScheme) -> Theme {
        match scheme {
            ColorScheme::Dark => self.dark,
            ColorScheme::Light => self.light,
        }
    }
}

impl Default for ThemeSet {
    fn default() -> Self {
        Self {
            dark: default_theme_from_colors(default_dark_colors()),
            light: default_theme_from_colors(default_light_colors()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppSettings {
    pub themes: ThemeSet,
    pub show_hotkey: Option<String>,
    pub max_title_length: usize,
    pub max_combined_title_length: usize,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            themes: ThemeSet::default(),
            show_hotkey: None,
            max_title_length: default_max_title_length(),
            max_combined_title_length: default_max_combined_title_length(),
        }
    }
}

pub fn load_settings() -> AppSettings {
    match try_load_settings() {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load config; using default settings");
            AppSettings::default()
        }
    }
}

fn try_load_settings() -> Result<AppSettings> {
    let Some(path) = default_config_path() else {
        return Ok(AppSettings::default());
    };

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AppSettings::default()),
        Err(e) => {
            return Err(e).with_context(|| format!("read config file {}", path.display()));
        }
    };

    let config: AppConfig = serde_json::from_str(&text)
        .with_context(|| format!("parse config file {} as JSON", path.display()))?;

    Ok(AppSettings {
        themes: ThemeSet {
            dark: build_theme(&config.colors.dark, "colors.dark")?,
            light: build_theme(&config.colors.light, "colors.light")?,
        },
        show_hotkey: config.menu.show_hotkey,
        max_title_length: config.menu.max_title_length,
        max_combined_title_length: config.menu.max_combined_title_length,
    })
}

fn default_config_path() -> Option<PathBuf> {
    let app_data = env::var_os("APPDATA")?;
    Some(PathBuf::from(app_data).join("komorebi-tray-grid").join("config.json"))
}

fn build_theme(config: &ColorConfig, scope: &str) -> Result<Theme> {
    Ok(Theme {
        focused: parse_hex_color(&config.focused)
            .with_context(|| format!("parse {scope}.focused as hex color"))?,
        non_empty_1: parse_hex_color(&config.non_empty_1)
            .with_context(|| format!("parse {scope}.non_empty_1 as hex color"))?,
        non_empty_2: parse_hex_color(&config.non_empty_2)
            .with_context(|| format!("parse {scope}.non_empty_2 as hex color"))?,
        non_empty_3_plus: parse_hex_color(&config.non_empty_3_plus)
            .with_context(|| format!("parse {scope}.non_empty_3_plus as hex color"))?,
        full_screen_border: parse_hex_color(&config.full_screen_border)
            .with_context(|| format!("parse {scope}.full_screen_border as hex color"))?,
        active_monitor_border: parse_hex_color(&config.active_monitor_border)
            .with_context(|| format!("parse {scope}.active_monitor_border as hex color"))?,
        inactive_monitor_border: parse_hex_color(&config.inactive_monitor_border)
            .with_context(|| format!("parse {scope}.inactive_monitor_border as hex color"))?,
        empty: parse_hex_color(&config.empty)
            .with_context(|| format!("parse {scope}.empty as hex color"))?,
    })
}

fn default_dark_colors() -> ColorConfig {
    ColorConfig::default()
}

fn default_light_colors() -> ColorConfig {
    ColorConfig {
        focused: "#0067C0FF".to_string(),
        non_empty_1: "#868686FF".to_string(),
        non_empty_2: "#6A6A6AFF".to_string(),
        non_empty_3_plus: "#4F4F4FFF".to_string(),
        full_screen_border: "#C7A000FF".to_string(),
        active_monitor_border: "#0067C0FF".to_string(),
        inactive_monitor_border: "#6A6A6AFF".to_string(),
        empty: "#00000000".to_string(),
    }
}

fn default_theme_from_colors(config: ColorConfig) -> Theme {
    Theme {
        focused: parse_hex_color(&config.focused).expect("default focused must be valid"),
        non_empty_1: parse_hex_color(&config.non_empty_1)
            .expect("default non_empty_1 must be valid"),
        non_empty_2: parse_hex_color(&config.non_empty_2)
            .expect("default non_empty_2 must be valid"),
        non_empty_3_plus: parse_hex_color(&config.non_empty_3_plus)
            .expect("default non_empty_3_plus must be valid"),
        full_screen_border: parse_hex_color(&config.full_screen_border)
            .expect("default full_screen_border must be valid"),
        active_monitor_border: parse_hex_color(&config.active_monitor_border)
            .expect("default active_monitor_border must be valid"),
        inactive_monitor_border: parse_hex_color(&config.inactive_monitor_border)
            .expect("default inactive_monitor_border must be valid"),
        empty: parse_hex_color(&config.empty).expect("default empty must be valid"),
    }
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

fn default_max_title_length() -> usize {
    64
}

fn default_max_combined_title_length() -> usize {
    96
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