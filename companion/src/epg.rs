// TV guide for French DTT channels (xmltvfr.fr) and the "now playing" card.
use chrono::DateTime;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

use crate::store::{self, now_ms, App};

const EPG_URL: &str = "https://xmltvfr.fr/xmltv/xmltv_tnt.zip";
const EPG_XML: &str = "xmltv_tnt.xml";

#[derive(Serialize, Clone)]
pub struct Prog {
    pub start: i64,
    pub stop: i64,
    pub title: String,
    pub desc: String,
    pub category: String,
    pub image: String,
}

#[derive(Default)]
pub struct Epg {
    /// guide id -> (display name, logo)
    channels: HashMap<String, (String, String)>,
    /// normalized display name -> guide id
    by_name: HashMap<String, String>,
    progs: HashMap<String, Vec<Prog>>,
}

pub async fn run(app: Arc<App>) {
    loop {
        if let Err(e) = refresh(&app).await {
            log!("epg error: {e}");
        }
        tokio::time::sleep(Duration::from_secs(3 * 3600)).await;
    }
}

async fn refresh(app: &App) -> anyhow::Result<()> {
    let xml_path = store::path(EPG_XML);
    let age = std::fs::metadata(&xml_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or(Duration::MAX);
    if age > Duration::from_secs(12 * 3600) {
        let bytes = reqwest::get(EPG_URL).await?.error_for_status()?.bytes().await?;
        let xml_path = xml_path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            let mut file = zip.by_name(EPG_XML)?;
            let mut xml = String::new();
            file.read_to_string(&mut xml)?;
            std::fs::write(&xml_path, xml)?;
            Ok(())
        })
        .await??;
    }
    let epg = tokio::task::spawn_blocking(move || -> anyhow::Result<Epg> { Ok(parse(&std::fs::read_to_string(xml_path)?)) }).await??;
    log!("epg parsed {} channels", epg.progs.len());
    *app.epg.write().await = epg;
    Ok(())
}

fn norm(s: &str) -> String {
    s.nfd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).filter(|c| c.is_ascii_alphanumeric()).collect()
}

fn unxml(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&")
}

fn parse(xml: &str) -> Epg {
    let tag = |body: &str, name: &str| -> String {
        Regex::new(&format!(r"<{name}[^>]*>([\s\S]*?)</{name}>"))
            .ok()
            .and_then(|re| re.captures(body).map(|c| unxml(&c[1])))
            .unwrap_or_default()
    };
    let icon_re = Regex::new(r#"<icon src="([^"]+)""#).unwrap();
    let icon = |body: &str| icon_re.captures(body).map(|c| c[1].replacen("http:", "https:", 1)).unwrap_or_default();
    let date = |d: &str, tz: &str| DateTime::parse_from_str(&format!("{d} {tz}"), "%Y%m%d%H%M%S %z").map(|t| t.timestamp_millis()).ok();

    let mut epg = Epg::default();
    for c in Regex::new(r#"<channel id="([^"]+)">([\s\S]*?)</channel>"#).unwrap().captures_iter(xml) {
        let name = tag(&c[2], "display-name");
        epg.by_name.insert(norm(&name), c[1].to_string());
        epg.channels.insert(c[1].to_string(), (name, icon(&c[2])));
    }
    let now = now_ms();
    let re = Regex::new(r#"<programme start="(\d{14}) ([+-]\d{4})" stop="(\d{14}) ([+-]\d{4})" channel="([^"]+)"[^>]*>([\s\S]*?)</programme>"#).unwrap();
    for c in re.captures_iter(xml) {
        let (Some(start), Some(stop)) = (date(&c[1], &c[2]), date(&c[3], &c[4])) else { continue };
        if stop < now - 3_600_000 || start > now + 36 * 3_600_000 {
            continue;
        }
        let body = &c[6];
        epg.progs.entry(c[5].to_string()).or_default().push(Prog {
            start,
            stop,
            title: tag(body, "title"),
            desc: tag(body, "desc"),
            category: tag(body, "category"),
            image: icon(body),
        });
    }
    for list in epg.progs.values_mut() {
        list.sort_by_key(|p| p.start);
    }
    epg
}

impl Epg {
    /// LG channel name -> guide id
    pub fn channel_id(&self, name: &str) -> Option<String> {
        let n = norm(name);
        if let Some(id) = self.by_name.get(&n) {
            return Some(id.clone());
        }
        let alias = match n.as_str() {
            "tfx" => Some("NT1.fr"),
            "rmcstory" => Some("Numero23.fr"),
            "rmclife" => Some("Cherie25.fr"),
            "lequipe" => Some("LEquipe21.fr"),
            "franceinfo" => Some("FranceInfo.fr"),
            "artetv" => Some("Arte.fr"),
            _ if n.starts_with("f3") || n.starts_with("france3") => Some("France3.fr"),
            _ => None,
        };
        if let Some(id) = alias {
            return Some(id.into());
        }
        self.by_name.iter().find(|(k, _)| k.starts_with(&n) || n.starts_with(k.as_str())).map(|(_, id)| id.clone())
    }
}

/// Full guide for the phone and the TV: the TV's channels in its own order, each with its programmes from now on.
pub async fn guide(app: &App) -> Value {
    let channels = app.state.lock().await.channels.clone();
    let epg = app.epg.read().await;
    let t = now_ms();
    let mut seen = std::collections::HashSet::new();
    let list: Vec<Value> = channels.iter().filter_map(|ch| {
        let id = epg.channel_id(&ch.name);
        // Same guide twice (Arte and Arte.tv): keep the first, the real channel
        if let Some(id) = &id { if !seen.insert(id.clone()) { return None; } }
        let logo = id.as_ref().and_then(|id| epg.channels.get(id)).map(|c| c.1.clone()).unwrap_or_default();
        let progs: Vec<Value> = id.as_ref().and_then(|id| epg.progs.get(id)).map(|l| l.iter().filter(|p| p.stop > t).map(|p| {
            let desc: String = p.desc.chars().take(400).collect();
            json!({ "start": p.start, "stop": p.stop, "title": p.title, "desc": desc, "category": p.category, "image": p.image })
        }).collect()).unwrap_or_default();
        Some(json!({ "number": ch.number, "name": ch.name, "id": ch.id, "guide": id, "logo": logo, "progs": progs }))
    }).collect();
    json!({ "channels": list, "time": t })
}

/// One programme by guide channel id and start time (to set a reminder).
pub async fn find(app: &App, guide: &str, start: i64) -> Option<(Prog, String)> {
    let epg = app.epg.read().await;
    let name = epg.channels.get(guide).map(|c| c.0.clone()).unwrap_or_default();
    epg.progs.get(guide)?.iter().find(|p| p.start == start).map(|p| (p.clone(), name))
}

/// What the Live TV card shows: last watched channel, current and next programme.
pub async fn live_info(app: &App) -> Value {
    let Some(ch) = app.state.lock().await.last_channel.clone() else { return json!({ "channel": null }) };
    let epg = app.epg.read().await;
    let id = epg.channel_id(&ch.name);
    let empty = Vec::new();
    let list = id.as_ref().and_then(|id| epg.progs.get(id)).unwrap_or(&empty);
    let t = now_ms();
    let now = list.iter().find(|p| p.start <= t && p.stop > t);
    let after = now.map(|p| p.stop).unwrap_or(t);
    let next = list.iter().find(|p| p.start >= after);
    let logo = id.as_ref().and_then(|id| epg.channels.get(id)).map(|c| c.1.clone()).unwrap_or_default();
    json!({ "channel": { "number": ch.number, "name": ch.name, "logo": logo }, "now": now, "next": next, "time": t })
}
