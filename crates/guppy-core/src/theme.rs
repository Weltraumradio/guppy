//! Colors from the active Omarchy theme (`colors.toml`), falling back to Matte Black.

use serde::Deserialize;
use std::path::PathBuf;

/// RGB color as `0xRRGGBB`.
pub type Rgb = u32;

#[derive(Debug, Clone)]
pub struct Theme {
    pub accent: Rgb,
    pub background: Rgb,
    pub dark_background: Rgb,
    pub lighter_background: Rgb,
    pub selection: Rgb,
    pub muted: Rgb,
    pub foreground: Rgb,
    pub light_foreground: Rgb,
    pub bright: Rgb,
    pub red: Rgb,
    pub marked: Rgb,
}

impl Default for Theme {
    /// Omarchy "Matte Black".
    fn default() -> Self {
        Self {
            accent: 0xe68e0d,
            background: 0x121212,
            dark_background: 0x0d0d0d,
            lighter_background: 0x1e1e1e,
            selection: 0x2a2a2a,
            muted: 0x333333,
            foreground: 0xbebebe,
            light_foreground: 0x8a8a8d,
            bright: 0xeaeaea,
            red: 0xd35f5f,
            marked: 0xffc107,
        }
    }
}

#[derive(Deserialize, Default)]
struct Raw {
    accent: Option<String>,
    background: Option<String>,
    dark_background: Option<String>,
    lighter_background: Option<String>,
    selection: Option<String>,
    muted: Option<String>,
    foreground: Option<String>,
    light_foreground: Option<String>,
    bright_cyan: Option<String>,
    red: Option<String>,
    bright_green: Option<String>,
}

fn hex(s: &Option<String>) -> Option<Rgb> {
    u32::from_str_radix(s.as_deref()?.trim_start_matches('#'), 16).ok()
}

impl Theme {
    pub fn omarchy_path() -> Option<PathBuf> {
        dirs::state_dir().map(|d| d.join("omarchy/current/theme/colors.toml"))
    }

    /// Loads the Omarchy theme; missing values come from the default.
    pub fn load() -> Self {
        Self::omarchy_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    pub fn parse(src: &str) -> Self {
        let raw: Raw = toml::from_str(src).unwrap_or_default();
        let d = Self::default();
        Self {
            accent: hex(&raw.accent).unwrap_or(d.accent),
            background: hex(&raw.background).unwrap_or(d.background),
            dark_background: hex(&raw.dark_background).unwrap_or(d.dark_background),
            lighter_background: hex(&raw.lighter_background).unwrap_or(d.lighter_background),
            selection: hex(&raw.selection).unwrap_or(d.selection),
            muted: hex(&raw.muted).unwrap_or(d.muted),
            foreground: hex(&raw.foreground).unwrap_or(d.foreground),
            light_foreground: hex(&raw.light_foreground).unwrap_or(d.light_foreground),
            bright: hex(&raw.bright_cyan).unwrap_or(d.bright),
            red: hex(&raw.red).unwrap_or(d.red),
            marked: hex(&raw.bright_green).unwrap_or(d.marked),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial() {
        let t = Theme::parse("accent = \"#112233\"\n");
        assert_eq!(t.accent, 0x112233);
        assert_eq!(t.background, 0x121212);
    }
}
