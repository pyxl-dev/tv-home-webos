// TV colour themes: presets, plus a custom one (background + accent, dark or light).
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Theme {
    /// Preset key, or "custom"
    pub name: String,
    pub bg: String,
    pub accent: String,
    pub light: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Theme { name: "nuit".into(), bg: "#2a0b1e".into(), accent: "#ff6fae".into(), light: false }
    }
}

#[derive(Serialize)]
pub struct Preset {
    pub key: &'static str,
    pub name: &'static str,
    pub bg: &'static str,
    pub accent: &'static str,
    pub light: bool,
}

const fn p(key: &'static str, name: &'static str, bg: &'static str, accent: &'static str, light: bool) -> Preset {
    Preset { key, name, bg, accent, light }
}

/// Is this theme name still valid (a saved theme may have been deleted)?
pub fn known(name: &str, saved: &[UserTheme]) -> bool {
    name == "custom" || PRESETS.iter().any(|p| p.key == name) || saved.iter().any(|t| t.key == name)
}

pub const PRESETS: &[Preset] = &[
    p("nuit", "Nuit", "#000000", "#ffffff", false),
    p("rose", "Rose", "#2a0b1e", "#ff6fae", false),
    p("roseclair", "Rose clair", "#ffe1ec", "#d6336c", true),
    p("lavande", "Lavande", "#150d29", "#b48cff", false),
    p("ocean", "Océan", "#03111f", "#4db2ff", false),
    p("foret", "Forêt", "#06150d", "#3ccf82", false),
    p("sable", "Sable", "#f4ede2", "#b5651d", true),
];

/// A theme saved by the user from the custom editor.
#[derive(Serialize, Deserialize, Clone)]
pub struct UserTheme {
    pub key: String,
    pub name: String,
    pub bg: String,
    pub accent: String,
    #[serde(default)]
    pub light: bool,
}

pub const MAX_USER_THEMES: usize = 24;

pub fn hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Validate a theme sent by the TV or the phone. `name` is a preset key, a saved theme key, or "custom".
pub fn parse(v: &serde_json::Value, saved: &[UserTheme]) -> Option<Theme> {
    let name = v["name"].as_str()?;
    if name == "custom" {
        let (bg, accent) = (v["bg"].as_str()?, v["accent"].as_str()?);
        if !hex(bg) || !hex(accent) {
            return None;
        }
        return Some(Theme { name: name.into(), bg: bg.to_lowercase(), accent: accent.to_lowercase(), light: v["light"].as_bool().unwrap_or(false) });
    }
    // A preset keeps the custom colours it was sent with, so switching back to "Perso" restores them
    let d = Theme::default();
    let keep = |k: &str, def: String| v[k].as_str().filter(|s| hex(s)).map(str::to_lowercase).unwrap_or(def);
    let known = PRESETS.iter().any(|p| p.key == name) || saved.iter().any(|t| t.key == name);
    known.then(|| Theme {
        name: name.into(),
        bg: keep("bg", d.bg.clone()),
        accent: keep("accent", d.accent.clone()),
        light: v["light"].as_bool().unwrap_or(d.light),
    })
}
