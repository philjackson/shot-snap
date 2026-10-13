use std::path::PathBuf;

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub save_to_file: bool,
    /// Empty = XDG_PICTURES_DIR, falling back to ~/Pictures.
    pub directory: String,
    /// strftime pattern; `.png` is appended. Empty = `screenshot_%Y%m%d_%H%M%S`.
    pub filename_pattern: String,
    pub copy_to_clipboard: bool,
    pub freeze_screen: bool,
    pub confirm_region: bool,
    pub remember_last_region: bool,
    pub show_cursor: bool,
    /// Run through `/bin/sh -lc` with the PNG on stdin. Empty = off.
    pub pipe_command: String,
    /// Show a notification, with a preview, when a screenshot is copied to the clipboard.
    pub notify: bool,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_to_file: true,
            directory: String::new(),
            filename_pattern: String::new(),
            copy_to_clipboard: true,
            freeze_screen: true,
            confirm_region: false,
            remember_last_region: false,
            show_cursor: false,
            pipe_command: String::new(),
            notify: true,
            theme: Theme::default(),
        }
    }
}

/// Colours default to Noctalia's built-in "Noctalia" palette.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub primary: Color,
    pub on_primary: Color,
    pub surface: Color,
    pub on_surface: Color,
    pub surface_variant: Color,
    pub outline: Color,
    pub hover: Color,
    pub on_hover: Color,
    pub dim_opacity: f32,
    /// fontconfig pattern, resolved with `fc-match`.
    pub font: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            primary: Color::hex("#fff59b"),
            on_primary: Color::hex("#0e0e43"),
            surface: Color::hex("#070722"),
            on_surface: Color::hex("#f3edf7"),
            surface_variant: Color::hex("#11112d"),
            outline: Color::hex("#21215f"),
            hover: Color::hex("#9bfece"),
            on_hover: Color::hex("#0e0e43"),
            dim_opacity: 0.65,
            font: "sans-serif:bold".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    fn hex(s: &str) -> Self {
        Self::parse(s).expect("valid built-in colour")
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#')?;
        let channel = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok().map(|v| f32::from(v) / 255.0);
        match s.len() {
            6 => Some(Self { r: channel(0)?, g: channel(2)?, b: channel(4)?, a: 1.0 }),
            8 => Some(Self { r: channel(0)?, g: channel(2)?, b: channel(4)?, a: channel(6)? }),
            _ => None,
        }
    }

    /// Rounded to 8 bits per channel, which is what the state file stores.
    pub fn quantized(self) -> Self {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0;
        Self { r: q(self.r), g: q(self.g), b: q(self.b), a: q(self.a) }
    }

    pub fn to_hex(self) -> String {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if b(self.a) == 255 {
            format!("#{:02x}{:02x}{:02x}", b(self.r), b(self.g), b(self.b))
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", b(self.r), b(self.g), b(self.b), b(self.a))
        }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    pub fn fv(self) -> femtovg::Color {
        femtovg::Color::rgbaf(self.r, self.g, self.b, self.a)
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid colour {s:?}, expected #RRGGBB or #RRGGBBAA")))
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(path)
    }
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("shot-snap/config.toml")
}

impl Config {
    /// Missing file = defaults. A broken file is reported and also falls back to defaults, so a
    /// typo never leaves you unable to take a screenshot.
    pub fn load() -> (Self, Option<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    pub fn output_directory(&self) -> PathBuf {
        if !self.directory.is_empty() {
            return expand_tilde(&self.directory);
        }
        if let Some(dir) = std::env::var_os("XDG_PICTURES_DIR").filter(|v| !v.is_empty()) {
            return expand_tilde(&dir.to_string_lossy());
        }
        if let Some(dir) = pictures_from_user_dirs() {
            return dir;
        }
        home().join("Pictures")
    }

    pub fn filename_pattern(&self) -> &str {
        if self.filename_pattern.is_empty() {
            "screenshot_%Y%m%d_%H%M%S"
        } else {
            &self.filename_pattern
        }
    }
}

/// Reads XDG_PICTURES_DIR from `user-dirs.dirs` (format: `XDG_PICTURES_DIR="$HOME/Pictures"`).
fn pictures_from_user_dirs() -> Option<PathBuf> {
    let text = std::fs::read_to_string(xdg_dir("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs")).ok()?;
    let line = text.lines().map(str::trim).find(|l| l.starts_with("XDG_PICTURES_DIR="))?;
    let value = line["XDG_PICTURES_DIR=".len()..].trim_matches('"');
    let value = value.replace("$HOME", &home().to_string_lossy());
    Some(PathBuf::from(value))
}
