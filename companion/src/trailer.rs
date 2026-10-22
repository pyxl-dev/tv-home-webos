// Trailers for the TV sheet: French dub (VF) first, else original with French subtitles (VOST), else original (VO).
// TMDB lists the official ones; YouTube search fills the gaps (many French channels post VF/VOSTFR trailers).
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::store::App;
use crate::suggest::{fold, tmdb, urlencode};
use crate::tmdb::DAY;

const TTL: Duration = Duration::from_secs(2 * 86400);

#[derive(Serialize, Clone)]
pub struct Trailer {
    /// YouTube video id
    pub key: String,
    /// "VF", "VOST" or "VO"
    pub lang: &'static str,
    pub name: String,
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36")
        .build().expect("http client"))
}

fn cache() -> &'static Mutex<HashMap<String, (Instant, Option<Trailer>)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Option<Trailer>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Videos whose owner forbids embedding (player error 101/150), reported by the TV and never picked again.
fn blocked() -> &'static Mutex<HashSet<String>> {
    static B: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

/// Forget `key` and drop the cached picks that used it, so the next lookup finds another video.
pub async fn block(key: &str) {
    blocked().lock().await.insert(key.to_string());
    cache().lock().await.retain(|_, (_, t)| t.as_ref().map_or(true, |t| t.key != key));
    log!("trailer {key} blocked (no embedding)");
}

/// Letters and digits only, so "Aquaman et le Royaume perdu" matches "AQUAMAN ET LE ROYAUME PERDU | Bande annonce".
fn squash(s: &str) -> String {
    fold(s).chars().filter(|c| c.is_alphanumeric()).collect()
}

fn has_word(name: &str, words: &[&str]) -> bool {
    fold(name).split(|c: char| !c.is_alphanumeric()).any(|w| words.contains(&w))
}

fn is_vost(name: &str) -> bool {
    has_word(name, &["vost", "vostfr", "vosta", "vo-st", "sous-titre", "soustitre"]) || fold(name).contains("sous-titr")
}

/// Best YouTube trailer on TMDB for a language: official trailers first, then teasers.
fn pick_tmdb(v: &Value, skip: &HashSet<String>, want: impl Fn(&str) -> bool) -> Option<Trailer> {
    let list = v["results"].as_array()?;
    let rank = |x: &Value| (x["type"] != "Trailer", x["official"] != true);
    list.iter()
        .filter(|x| x["site"] == "YouTube" && (x["type"] == "Trailer" || x["type"] == "Teaser"))
        .filter(|x| !skip.contains(x["key"].as_str().unwrap_or("")))
        .filter(|x| want(x["name"].as_str().unwrap_or("")))
        .min_by_key(|x| rank(x))
        .map(|x| Trailer { key: x["key"].as_str().unwrap_or("").into(), lang: "", name: x["name"].as_str().unwrap_or("").into() })
}

/// First YouTube search result that names the title, is a short video and passes `want`.
/// For series, spin-off films ("One Piece : Le film") are skipped.
async fn youtube(query: &str, title: &str, series: bool, skip: &HashSet<String>, want: impl Fn(&str) -> bool) -> Option<Trailer> {
    let url = format!("https://www.youtube.com/results?hl=fr&gl=FR&search_query={}", urlencode(query));
    let html = client().get(url).header("Accept-Language", "fr-FR,fr").send().await.ok()?.text().await.ok()?;
    let start = html.find("var ytInitialData = ")? + "var ytInitialData = ".len();
    let end = start + html[start..].find(";</script>")?;
    let data: Value = serde_json::from_str(&html[start..end]).ok()?;
    let mut videos = Vec::new();
    collect(&data, &mut videos);
    let t = squash(title);
    videos.into_iter().take(10).find_map(|v| {
        if skip.contains(v["videoId"].as_str()?) { return None }
        let name = v["title"]["runs"][0]["text"].as_str()?;
        let secs = v["lengthText"]["simpleText"].as_str()?.split(':').try_fold(0u32, |a, p| p.parse::<u32>().ok().map(|n| a * 60 + n))?;
        let fan = ["fanmade", "fan made", "fan-made", "parodie", "reaction", "concept"].iter().any(|w| fold(name).contains(w));
        let film = series && has_word(name, &["film", "movie"]);
        let fits = (30..=360).contains(&secs) && !fan && !film && squash(name).contains(&t) && want(name)
            && (fold(name).contains("bande") || fold(name).contains("trailer") || fold(name).contains("teaser"));
        fits.then(|| Trailer { key: v["videoId"].as_str().unwrap_or("").into(), lang: "", name: name.into() })
    })
}

fn collect<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            if let Some(r) = m.get("videoRenderer") { out.push(r) }
            m.values().for_each(|x| collect(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

pub async fn find(app: &App, kind: &'static str, id: i64) -> anyhow::Result<Option<Trailer>> {
    let ck = format!("{kind}:{id}");
    if let Some((at, t)) = cache().lock().await.get(&ck) {
        if at.elapsed() < TTL { return Ok(t.clone()) }
    }
    let (fq, eq, dq) = (format!("/{kind}/{id}/videos?language=fr-FR"), format!("/{kind}/{id}/videos?language=en-US"), format!("/{kind}/{id}?language=fr-FR"));
    let (fr, d) = tokio::try_join!(tmdb(app, &fq, DAY), tmdb(app, &dq, 7 * DAY))?;
    let title = d["title"].as_str().or(d["name"].as_str()).unwrap_or("").to_string();
    let year: String = d["release_date"].as_str().or(d["first_air_date"].as_str()).unwrap_or("").chars().take(4).collect();
    let skip = blocked().lock().await.clone();
    let tag = |t: Option<Trailer>, lang| t.filter(|t| !t.key.is_empty()).map(|t| Trailer { lang, ..t });

    // TMDB's French videos without "VF"/"VOST" in the name are dubbed for most titles, but usually subtitled for Asian ones
    let asian = matches!(d["original_language"].as_str(), Some("ja" | "ko" | "zh" | "cn"));
    let vf_name = |n: &str| has_word(n, &["vf", "vff", "francais", "french"]) && !is_vost(n);
    let mut found = tag(pick_tmdb(&fr, &skip, vf_name), "VF");
    if found.is_none() && !asian { found = tag(pick_tmdb(&fr, &skip, |n| !is_vost(n)), "VF") }
    if found.is_none() && !title.is_empty() {
        found = tag(youtube(&format!("{title} {year} bande annonce VF"), &title, kind == "tv", &skip, vf_name).await, "VF");
    }
    if found.is_none() { found = tag(pick_tmdb(&fr, &skip, |n| is_vost(n) || (asian && !vf_name(n))), "VOST") }
    if found.is_none() && !title.is_empty() {
        found = tag(youtube(&format!("{title} {year} bande annonce VOSTFR"), &title, kind == "tv", &skip, is_vost).await, "VOST");
    }
    if found.is_none() {
        found = tag(pick_tmdb(&*tmdb(app, &eq, DAY).await?, &skip, |_| true), "VO");
    }
    log!("trailer {ck}: {}", found.as_ref().map(|t| format!("{} {} ({})", t.lang, t.key, t.name)).unwrap_or("none".into()));
    cache().lock().await.insert(ck, (Instant::now(), found.clone()));
    Ok(found)
}
