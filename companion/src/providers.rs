// Streaming services, in default priority order. `ids` are TMDB provider ids (FR),
// `app` the webOS app id, `pattern` finds the app among the ones opened on the TV.
use regex::Regex;
use std::collections::HashMap;

use crate::store::{PublicProvider, Settings};

pub struct Provider {
    pub key: &'static str,
    pub ids: &'static [i64],
    pub name: &'static str,
    pub color: &'static str,
    pub app: Option<&'static str>,
    pub pattern: Option<&'static str>,
}

const fn p(key: &'static str, ids: &'static [i64], name: &'static str, color: &'static str,
           app: Option<&'static str>, pattern: Option<&'static str>) -> Provider {
    Provider { key, ids, name, color, app, pattern }
}

pub const CATALOG: &[Provider] = &[
    p("netflix", &[8], "Netflix", "#e50914", Some("netflix"), None),
    p("prime", &[119, 9], "Prime Video", "#1a98ff", Some("amazon"), None),
    p("disney", &[337], "Disney+", "#3c8cff", Some("com.disney.disneyplus-prod"), None),
    p("canal", &[381], "Canal+", "#ffffff", None, Some("canal")),
    p("appletv", &[350], "Apple TV+", "#ffffff", Some("com.apple.appletv"), None),
    p("max", &[1899], "Max", "#4d7cff", None, Some(r"hbo|\.max|max\.")),
    p("paramount", &[531, 2303], "Paramount+", "#2f7bff", None, Some("paramount")),
    p("crunchyroll", &[283], "Crunchyroll", "#f47521", None, Some("crunchyroll")),
    p("arte", &[234], "Arte", "#fa481c", None, Some("arte")),
    p("tf1", &[1754], "TF1+", "#2a9dff", None, Some("tf1")),
    p("m6", &[147], "M6+", "#ffffff", None, Some("m6|6play")),
    p("molotov", &[1967], "Molotov", "#ffd400", None, Some("molotov")),
];

pub fn entry(key: &str) -> Option<&'static Provider> {
    CATALOG.iter().find(|p| p.key == key)
}

/// Enabled services, in the user's priority order.
pub fn enabled(settings: &Settings) -> Vec<&'static Provider> {
    settings.providers.iter().filter(|x| x.on).filter_map(|x| entry(&x.key)).collect()
}

pub fn public(p: &Provider, seen_apps: &HashMap<String, i64>) -> PublicProvider {
    let app = p.app.map(String::from).or_else(|| {
        let re = Regex::new(p.pattern?).ok()?;
        seen_apps.keys().find(|id| re.is_match(id)).cloned()
    });
    PublicProvider {
        key: p.key.into(),
        name: p.name.into(),
        color: p.color.into(),
        app,
        also: Vec::new(),
        on: None,
    }
}
