// Shared state and the JSON files in data/ (same formats as the former Node companion).
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicI64;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Notify, RwLock};

use crate::{epg::Epg, providers, reminders::Reminder, theme::{self, Theme, UserTheme}, tmdb::Tmdb};

pub const DATA: &str = "data";

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn path(name: &str) -> PathBuf {
    PathBuf::from(DATA).join(name)
}

pub fn load<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    let text = std::fs::read_to_string(path(name)).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            log!("load {name}: {e}");
            None
        }
    }
}

/// Atomic write: temp file then rename, so a crash never leaves a half-written file.
pub fn save<T: Serialize>(name: &str, value: &T) {
    let file = path(name);
    let tmp = file.with_extension("json.tmp");
    let res = serde_json::to_string_pretty(value)
        .map_err(anyhow::Error::from)
        .and_then(|s| std::fs::write(&tmp, s).map_err(Into::into))
        .and_then(|_| std::fs::rename(&tmp, &file).map_err(Into::into));
    if let Err(e) = res {
        log!("save {name}: {e}");
    }
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Channel {
    pub number: String,
    pub name: String,
    pub id: String,
}

#[derive(Serialize, Clone, Default)]
pub struct TvLive {
    pub app: String,
    pub volume: Option<i64>,
    pub muted: bool,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub last_channel: Option<Channel>,
    pub seen_apps: HashMap<String, i64>,
    pub channels: Vec<Channel>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct ProviderPref {
    pub key: String,
    pub on: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub tmdb_key: String,
    pub region: String,
    pub home_redirect: bool,
    pub auto_refresh: bool,
    pub rotate_seconds: u32,
    /// Minutes before a TNT programme that its reminder shows on the TV
    pub reminder_lead: u32,
    /// Seasonal picks (Halloween, Noël…) mixed into the suggestions
    pub seasonal: bool,
    /// Also suggest well-matched titles that none of the user's services carry
    pub off_platform: bool,
    #[serde(deserialize_with = "lenient_providers")]
    pub providers: Vec<ProviderPref>,
    #[serde(deserialize_with = "lenient_theme")]
    pub theme: Theme,
    #[serde(deserialize_with = "lenient_user_themes")]
    pub user_themes: Vec<UserTheme>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tmdb_key: String::new(),
            region: "FR".into(),
            home_redirect: false,
            auto_refresh: true,
            rotate_seconds: 10,
            reminder_lead: 15,
            seasonal: true,
            off_platform: false,
            providers: Vec::new(),
            theme: Theme::default(),
            user_themes: Vec::new(),
        }
    }
}

// An unreadable provider list must not wipe the rest of the settings (e.g. the TMDB key)
fn lenient_providers<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ProviderPref>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

fn lenient_theme<'de, D: Deserializer<'de>>(d: D) -> Result<Theme, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

fn lenient_user_themes<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<UserTheme>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

impl Settings {
    /// Keep the provider list in sync with the catalog: unknown keys dropped, new services appended (off).
    pub fn normalize(&mut self) {
        if self.providers.is_empty() {
            self.providers = providers::CATALOG
                .iter()
                .map(|p| ProviderPref { key: p.key.into(), on: p.key == "netflix" || p.key == "prime" })
                .collect();
        }
        self.providers.retain(|x| providers::entry(&x.key).is_some());
        self.user_themes.retain(|t| theme::hex(&t.bg) && theme::hex(&t.accent));
        if !theme::known(&self.theme.name, &self.user_themes) {
            self.theme.name = "nuit".into();
        }
        for p in providers::CATALOG {
            if !self.providers.iter().any(|x| x.key == p.key) {
                self.providers.push(ProviderPref { key: p.key.into(), on: false });
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Rating {
    pub verdict: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub genres: Vec<i64>,
    #[serde(default)]
    pub poster: String,
    #[serde(default)]
    pub at: i64,
}

#[derive(Serialize, Clone)]
pub struct PublicProvider {
    pub key: String,
    pub name: String,
    pub color: String,
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
}

#[derive(Serialize, Clone)]
pub struct Watch {
    pub name: String,
    pub mine: bool,
}

/// A film or series as sent to the TV and the phone.
#[derive(Serialize, Clone)]
pub struct Item {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: i64,
    pub title: String,
    pub year: String,
    pub genres: Vec<String>,
    pub genre_ids: Vec<i64>,
    pub overview: String,
    pub vote: f64,
    pub backdrop: String,
    pub poster: String,
    pub provider: Option<PublicProvider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch: Option<Vec<Watch>>,
    /// Set on seasonal picks ("Spécial Halloween")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<SeasonTag>,
    /// Set on a liked title brought back for the pleasure of watching it again
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub rewatch: bool,
}

#[derive(Serialize, Clone)]
pub struct SeasonTag {
    pub key: String,
    pub label: String,
    pub color: String,
}

#[derive(Serialize, Clone, Default)]
pub struct Suggestions {
    pub at: i64,
    pub items: Vec<Item>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct App {
    pub settings: RwLock<Settings>,
    pub state: Mutex<State>,
    pub ratings: RwLock<HashMap<String, Rating>>,
    pub apps: RwLock<Option<Vec<String>>>,
    pub suggestions: RwLock<Suggestions>,
    pub compute_lock: Mutex<()>,
    pub recompute: Notify,
    pub genres: RwLock<HashMap<i64, String>>,
    pub epg: RwLock<Epg>,
    pub reminders: Mutex<Vec<Reminder>>,
    pub tmdb: Tmdb,
    /// Outgoing SSAP messages, while the TV is connected
    pub tv_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<String>>>,
    /// Until when the LG home is let through (ms since epoch)
    pub bypass_until: AtomicI64,
    /// Since when the TV screen is on (ms since epoch), 0 when off or unreachable
    pub tv_on_since: AtomicI64,
    /// What the phone remote shows: app on screen, volume
    pub tv_live: std::sync::Mutex<TvLive>,
    /// Waiting for the TV's answer to a screen capture: its image URL, or the error
    pub shot_reply: std::sync::Mutex<Option<oneshot::Sender<Result<String, String>>>>,
    /// Last capture of the TV screen (one at a time, shared by every viewer)
    pub screen: Mutex<Option<(Instant, Vec<u8>)>>,
    /// Button presses for the TV's pointer socket, when the TV grants it (arrows, OK, Back on any app)
    pub pointer_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<String>>>,
    /// New pairing asked from the phone: "", "waiting" (prompt on the TV), "ok", "refused" (no pointer even so), "failed"
    pub pair_state: std::sync::Mutex<&'static str>,
    /// Drop the TV session so it reconnects with a new client key
    pub tv_reset: Notify,
    pub started: Instant,
    /// Pushed to the TV over SSE ("settings", "suggestions") so it updates right away
    pub events: broadcast::Sender<&'static str>,
}

impl App {
    pub fn load() -> anyhow::Result<App> {
        std::fs::create_dir_all(DATA)?;
        let mut settings: Settings = load("settings.json").unwrap_or_default();
        settings.normalize();
        save("settings.json", &settings);

        // Old ratings were keyed by bare movie id
        let mut ratings: HashMap<String, Rating> = load("ratings.json").unwrap_or_default();
        let old: Vec<String> = ratings.keys().filter(|k| k.parse::<i64>().is_ok()).cloned().collect();
        for k in &old {
            if let Some(r) = ratings.remove(k) {
                ratings.insert(format!("movie:{k}"), r);
            }
        }
        if !old.is_empty() {
            save("ratings.json", &ratings);
        }

        Ok(App {
            settings: RwLock::new(settings),
            state: Mutex::new(load("state.json").unwrap_or_default()),
            ratings: RwLock::new(ratings),
            apps: RwLock::new(load("apps.json")),
            suggestions: RwLock::new(Suggestions::default()),
            compute_lock: Mutex::new(()),
            recompute: Notify::new(),
            genres: RwLock::new(HashMap::new()),
            epg: RwLock::new(Epg::default()),
            reminders: Mutex::new(load("reminders.json").unwrap_or_default()),
            tmdb: Tmdb::new(),
            tv_tx: std::sync::Mutex::new(None),
            bypass_until: AtomicI64::new(0),
            tv_on_since: AtomicI64::new(0),
            tv_live: std::sync::Mutex::new(TvLive::default()),
            shot_reply: std::sync::Mutex::new(None),
            screen: Mutex::new(None),
            pointer_tx: std::sync::Mutex::new(None),
            pair_state: std::sync::Mutex::new(""),
            tv_reset: Notify::new(),
            started: Instant::now(),
            events: broadcast::channel(16).0,
        })
    }

    /// Queue an SSAP message for the TV; false when the TV isn't connected.
    pub fn tv_send(&self, msg: String) -> bool {
        match self.tv_tx.lock().unwrap().as_ref() {
            Some(tx) => tx.send(msg).is_ok(),
            None => false,
        }
    }
}
