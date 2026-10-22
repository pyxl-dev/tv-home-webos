// HTTP API for the TV app and the phone page (same routes as the former Node companion).
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use futures_util::{Stream, StreamExt};
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use tokio_stream::wrappers::BroadcastStream;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::store::{self, now_ms, App, ProviderPref, Rating};
use crate::{epg, justwatch, providers, reminders, suggest, theme, trailer as trailers, tv};

const DEFAULT_PORT: u16 = 8787;
type S = State<Arc<App>>;

/// Any handler error becomes a JSON 500 instead of tearing anything down.
struct Fail(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for Fail {
    fn from(e: E) -> Self {
        Fail(e.into())
    }
}
impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        log!("http error: {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": self.0.to_string() }))).into_response()
    }
}
type R = Result<Response, Fail>;

fn ok(v: impl serde::Serialize) -> R {
    Ok(Json(v).into_response())
}
fn bad(msg: &str) -> R {
    Ok((StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response())
}
// Bodies are parsed leniently: a malformed body is treated as empty
fn body(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
}

pub async fn serve(app: Arc<App>) -> anyhow::Result<()> {
    let router = Router::new()
        .route("/", get(page))
        .route("/rate", get(page))
        .route("/api/live", get(live))
        .route("/api/guide", get(guide))
        .route("/api/reminders", get(reminders_get).post(reminder_add))
        .route("/api/reminders/:id", axum::routing::delete(reminder_delete))
        .route("/api/tune", post(tune))
        .route("/api/tv", get(tv_status))
        .route("/api/remote", post(remote))
        .route("/api/screen", get(screen))
        .route("/api/pair", post(pair))
        .route("/api/apps", get(apps_get).put(apps_put))
        .route("/api/settings", get(settings_get).put(settings_put))
        .route("/api/lghome", post(lg_home))
        .route("/api/suggestions", get(suggestions))
        .route("/api/rate", post(rate))
        .route("/api/rate/queue", get(rate_queue))
        .route("/api/search", get(search))
        .route("/api/trailer", get(trailer))
        .route("/player", get(player_page))
        .route("/tv/*path", get(tv_file))
        .route("/pwa/:file", get(pwa_file))
        .route("/sw.js", get(|| pwa_file(Path("sw.js".into()))))
        .route("/api/item", get(item))
        .route("/api/ratings", get(ratings))
        .route("/api/events", get(events))
        .route("/api/play", post(play))
        .route("/api/themes", post(theme_save))
        .route("/api/themes/:key", axum::routing::delete(theme_delete).put(theme_update))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))) })
        .layer(CorsLayer::permissive())
        .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .with_state(app);
    let port = std::env::var("TVHOME_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    log!("companion listening on {port}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn page() -> R {
    // Read at each request so the page can be edited without rebuilding
    Ok(Html(tokio::fs::read_to_string("public/rate.html").await?).into_response())
}

/// YouTube player for the TV's trailer overlay: an http page, since YouTube refuses embeds from the app's file:// origin.
async fn player_page() -> R {
    Ok(Html(tokio::fs::read_to_string("public/player.html").await?).into_response())
}

/// The TV app's apps.js and logos, so the phone remote shows the same app tiles.
async fn tv_file(Path(path): Path<String>) -> R {
    let mime = match path.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => return Ok(StatusCode::NOT_FOUND.into_response()),
    };
    if path.contains("..") || !(path == "apps.js" || path.starts_with("logos/")) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    match tokio::fs::read(format!("../app/{path}")).await {
        Ok(bytes) => Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response()),
        Err(_) => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

/// Manifest and icons, so the phone page can be added to the home screen as an app.
async fn pwa_file(Path(file): Path<String>) -> R {
    let mime = match file.rsplit('.').next() {
        Some("webmanifest") => "application/manifest+json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("js") => "text/javascript; charset=utf-8",
        _ => return Ok(StatusCode::NOT_FOUND.into_response()),
    };
    if file.contains("..") || file.contains('/') { return Ok(StatusCode::NOT_FOUND.into_response()); }
    match tokio::fs::read(format!("public/pwa/{file}")).await {
        Ok(bytes) => Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response()),
        Err(_) => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

async fn live(State(app): S) -> R {
    ok(epg::live_info(&app).await)
}

async fn guide(State(app): S) -> R {
    ok(epg::guide(&app).await)
}

async fn reminders_get(State(app): S) -> R {
    ok(&*app.reminders.lock().await)
}

async fn reminder_add(State(app): S, text: String) -> R {
    let b = body(&text);
    let (Some(guide), Some(start)) = (b["guide"].as_str(), b["start"].as_i64()) else { return bad("bad reminder") };
    if start <= now_ms() { return bad("already started"); }
    let Some(r) = reminders::add(&app, guide, start).await else { return bad("unknown programme") };
    log!("reminder set: {} on {}", r.title, r.channel);
    reminders_get(State(app)).await
}

async fn reminder_delete(State(app): S, Path(id): Path<String>) -> R {
    reminders::remove(&app, &id).await;
    reminders_get(State(app)).await
}

/// Switch the TV to a channel (LG channel id), whatever is on screen.
async fn tune(State(app): S, text: String) -> R {
    let b = body(&text);
    let Some(id) = b["channelId"].as_str().filter(|s| !s.is_empty()) else { return bad("bad channel") };
    log!("tune {id}");
    ok(json!({ "ok": app.tv_send(tv::ssap("request", "tune", "ssap://tv/openChannel", json!({ "channelId": id }))) }))
}

/// What the phone remote shows: power, app on screen, volume, last live channel.
async fn tv_status(State(app): S) -> R {
    let live = app.tv_live.lock().unwrap().clone();
    let channel = app.state.lock().await.last_channel.clone();
    // Where arrows / OK reach: every app with the pointer socket, otherwise only our own app
    let nav = if app.pointer_tx.lock().unwrap().is_some() { "all" } else { "home" };
    ok(json!({ "on": app.tv_on_since.load(Ordering::Relaxed) > 0, "app": live.app, "volume": live.volume, "muted": live.muted,
               "channel": channel.map(|c| json!({ "number": c.number, "name": c.name })),
               "nav": nav, "pair": *app.pair_state.lock().unwrap() }))
}

/// Live view of the TV screen for the phone: one JPEG per request, the page asks again as soon as it has drawn it.
async fn screen(State(app): S) -> R {
    if app.tv_on_since.load(Ordering::Relaxed) == 0 { return Ok((StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "tv off" }))).into_response()); }
    match tv::screen(&app).await {
        Ok(jpg) => Ok(([(header::CONTENT_TYPE, "image/jpeg")], jpg).into_response()),
        Err(e) => Ok((StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e.to_string() }))).into_response()),
    }
}

/// Ask the TV for a new pairing (prompt on the TV), hoping it then grants the pointer socket.
async fn pair(State(app): S) -> R {
    if *app.pair_state.lock().unwrap() == "waiting" { return ok(json!({ "ok": true })); }
    if !app.tv_send(tv::ssap("request", "pairtest", "ssap://com.webos.applicationManager/getForegroundAppInfo", json!({}))) {
        return ok(json!({ "ok": false }));
    }
    tokio::spawn(tv::pair(app.clone()));
    ok(json!({ "ok": true }))
}

/// Arrows, OK and Back: through the pointer socket when the TV grants it, otherwise relayed to our own app
/// over SSE (only when it is on screen: no other app can be steered from the network).
fn nav_key(app: &App, key: &str) -> Option<bool> {
    let (button, event) = match key {
        "up" => ("UP", "key-up"),
        "down" => ("DOWN", "key-down"),
        "left" => ("LEFT", "key-left"),
        "right" => ("RIGHT", "key-right"),
        "ok" => ("ENTER", "key-ok"),
        "back" => ("BACK", "key-back"),
        _ => return None,
    };
    if tv::button(app, button) { return Some(true); }
    let home = app.tv_on_since.load(Ordering::Relaxed) > 0 && app.tv_live.lock().unwrap().app == tv::HOME_APP;
    Some(home && app.events.send(event).is_ok())
}

/// Phone remote: power, volume, channels, media keys, launching apps, and arrows / OK / Back (see `nav_key`).
async fn remote(State(app): S, text: String) -> R {
    let b = body(&text);
    let cmd = b["cmd"].as_str().unwrap_or("");
    if cmd == "key" {
        let key = b["key"].as_str().unwrap_or("");
        let Some(sent) = nav_key(&app, key) else { return bad("unknown key") };
        return ok(json!({ "ok": sent }));
    }
    let req = |uri: &str, payload: Value| app.tv_send(tv::ssap("request", "remote", uri, payload));
    let sent = match cmd {
        "on" => {
            // In "Screen Saver" the TV is still on the network so Wake-on-LAN does nothing; a toast wakes the screen
            let asleep = app.tv_on_since.load(std::sync::atomic::Ordering::Relaxed) == 0;
            let toast = asleep && req("ssap://system.notifications/createToast", json!({ "message": "Allumée depuis le téléphone" }));
            tv::wake() || toast
        }
        "off" => req("ssap://system/turnOff", json!({})),
        "volumeUp" => req("ssap://audio/volumeUp", json!({})),
        "volumeDown" => req("ssap://audio/volumeDown", json!({})),
        "mute" => {
            let muted = app.tv_live.lock().unwrap().muted;
            req("ssap://audio/setMute", json!({ "mute": !muted }))
        }
        "channelUp" => req("ssap://tv/channelUp", json!({})),
        "channelDown" => req("ssap://tv/channelDown", json!({})),
        "play" => req("ssap://media.controls/play", json!({})),
        "pause" => req("ssap://media.controls/pause", json!({})),
        "rewind" => req("ssap://media.controls/rewind", json!({})),
        "fastForward" => req("ssap://media.controls/fastForward", json!({})),
        "launch" => match b["id"].as_str() {
            Some(id) if !id.is_empty() => app.tv_send(tv::launch_msg(id)),
            _ => return bad("bad app"),
        },
        _ => return bad("unknown command"),
    };
    log!("remote {cmd}{}", if sent { "" } else { " (tv unreachable)" });
    ok(json!({ "ok": sent }))
}

async fn apps_get(State(app): S) -> R {
    let mut seen: Vec<(String, i64)> = app.state.lock().await.seen_apps.iter().map(|(k, v)| (k.clone(), *v)).collect();
    seen.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let seen: Vec<String> = seen.into_iter().map(|(k, _)| k).collect();
    ok(json!({ "apps": *app.apps.read().await, "seen": seen }))
}

async fn apps_put(State(app): S, text: String) -> R {
    let Some(list) = body(&text)["apps"].as_array().cloned() else { return bad("apps must be an array") };
    let list: Vec<String> = list.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    store::save("apps.json", &list);
    *app.apps.write().await = Some(list.clone());
    ok(json!({ "apps": list }))
}

async fn public_settings(app: &App) -> Value {
    let s = app.settings.read().await;
    let seen = app.state.lock().await.seen_apps.clone();
    let list: Vec<_> = s.providers.iter().filter_map(|x| {
        let mut p = providers::public(providers::entry(&x.key)?, &seen);
        p.on = Some(x.on);
        Some(p)
    }).collect();
    json!({ "providers": list, "homeRedirect": s.home_redirect, "autoRefresh": s.auto_refresh,
            "rotateSeconds": s.rotate_seconds, "reminderLead": s.reminder_lead, "seasonal": s.seasonal, "offPlatform": s.off_platform,
            "season": suggest::season_now().map(|x| x.label), "hasKey": !s.tmdb_key.is_empty(),
            "theme": s.theme, "themes": all_themes(&s) })
}

/// Built-in presets first, then the user's saved themes (flagged `user`).
fn all_themes(s: &store::Settings) -> Vec<Value> {
    theme::PRESETS.iter().map(|p| json!(p))
        .chain(s.user_themes.iter().map(|t| json!({ "key": t.key, "name": t.name, "bg": t.bg, "accent": t.accent, "light": t.light, "user": true })))
        .collect()
}

async fn theme_save(State(app): S, text: String) -> R {
    let b = body(&text);
    let (Some(bg), Some(accent)) = (b["bg"].as_str(), b["accent"].as_str()) else { return bad("bad theme") };
    let name: String = b["name"].as_str().unwrap_or("").trim().chars().take(24).collect();
    if !theme::hex(bg) || !theme::hex(accent) || name.is_empty() {
        return bad("bad theme");
    }
    {
        let mut s = app.settings.write().await;
        if s.user_themes.len() >= theme::MAX_USER_THEMES {
            return bad("too many themes");
        }
        let t = theme::UserTheme { key: format!("u{}", now_ms()), name, bg: bg.to_lowercase(), accent: accent.to_lowercase(), light: b["light"].as_bool().unwrap_or(false) };
        // Select the new theme right away, keeping its colours as the custom ones
        s.theme = theme::Theme { name: t.key.clone(), bg: t.bg.clone(), accent: t.accent.clone(), light: t.light };
        s.user_themes.push(t);
        store::save("settings.json", &*s);
    }
    let _ = app.events.send("settings");
    ok(public_settings(&app).await)
}

/// Edit a saved theme (colours, light/dark, name); the TV follows live if it's the one in use.
async fn theme_update(State(app): S, Path(key): Path<String>, text: String) -> R {
    let b = body(&text);
    {
        let mut s = app.settings.write().await;
        let Some(t) = s.user_themes.iter_mut().find(|t| t.key == key) else { return bad("unknown theme") };
        if let Some(v) = b["bg"].as_str().filter(|v| theme::hex(v)) { t.bg = v.to_lowercase(); }
        if let Some(v) = b["accent"].as_str().filter(|v| theme::hex(v)) { t.accent = v.to_lowercase(); }
        if let Some(v) = b["light"].as_bool() { t.light = v; }
        let name: String = b["name"].as_str().unwrap_or("").trim().chars().take(24).collect();
        if !name.is_empty() { t.name = name; }
        let t = t.clone();
        if s.theme.name == key {
            s.theme = theme::Theme { name: t.key, bg: t.bg, accent: t.accent, light: t.light };
        }
        store::save("settings.json", &*s);
    }
    let _ = app.events.send("settings");
    ok(public_settings(&app).await)
}

async fn theme_delete(State(app): S, Path(key): Path<String>) -> R {
    {
        let mut s = app.settings.write().await;
        let Some(i) = s.user_themes.iter().position(|t| t.key == key) else { return bad("unknown theme") };
        let t = s.user_themes.remove(i);
        // Deleting the theme in use falls back to "Perso" with the same colours: nothing changes on screen
        if s.theme.name == key {
            s.theme = theme::Theme { name: "custom".into(), bg: t.bg, accent: t.accent, light: t.light };
        }
        store::save("settings.json", &*s);
    }
    let _ = app.events.send("settings");
    ok(public_settings(&app).await)
}

async fn settings_get(State(app): S) -> R {
    ok(public_settings(&app).await)
}

async fn settings_put(State(app): S, text: String) -> R {
    let b = body(&text);
    let mut providers_changed = false;
    {
        let mut s = app.settings.write().await;
        if let Some(list) = b["providers"].as_array() {
            let mut next: Vec<ProviderPref> = list.iter().filter_map(|x| {
                let key = x["key"].as_str()?;
                providers::entry(key)?;
                Some(ProviderPref { key: key.into(), on: x["on"].as_bool().unwrap_or(false) })
            }).collect();
            next.dedup_by(|a, b| a.key == b.key);
            let old = std::mem::replace(&mut s.providers, next);
            s.normalize();
            providers_changed = old != s.providers;
        }
        if let Some(v) = b["homeRedirect"].as_bool() { s.home_redirect = v; }
        if let Some(v) = b["autoRefresh"].as_bool() { s.auto_refresh = v; }
        if let Some(v) = b["seasonal"].as_bool() {
            providers_changed |= v != s.seasonal;
            s.seasonal = v;
        }
        if let Some(v) = b["offPlatform"].as_bool() {
            providers_changed |= v != s.off_platform;
            s.off_platform = v;
        }
        if let Some(t) = theme::parse(&b["theme"], &s.user_themes) { s.theme = t; }
        if let Some(v) = b["reminderLead"].as_u64() {
            if reminders::LEAD_CHOICES.contains(&(v as u32)) { s.reminder_lead = v as u32; }
        }
        if let Some(v) = b["rotateSeconds"].as_u64() {
            if [10, 15, 20, 30, 60].contains(&v) { s.rotate_seconds = v as u32; }
        }
        store::save("settings.json", &*s);
    }
    if providers_changed {
        app.recompute.notify_one();
    }
    let _ = app.events.send("settings");
    ok(public_settings(&app).await)
}

/// "Regarder": open the provider's app directly on the title when we can, else just the app.
async fn play(State(app): S, text: String) -> R {
    let b = body(&text);
    let kind = if b["type"] == "tv" { "tv" } else { "movie" };
    let (Some(id), Some(key)) = (b["id"].as_i64(), b["provider"].as_str()) else { return bad("bad play") };
    let Some(entry) = providers::entry(key) else { return bad("unknown provider") };
    let seen = app.state.lock().await.seen_apps.clone();
    let Some(app_id) = providers::public(entry, &seen).app else { return bad("app unknown") };
    let content = justwatch::content_id(key, kind, id, b["title"].as_str().unwrap_or("")).await;
    let mut payload = json!({ "id": app_id });
    if let Some(c) = &content { payload["contentId"] = json!(c); }
    log!("play {kind}:{id} on {app_id}{}", if content.is_some() { " (direct)" } else { "" });
    let sent = app.tv_send(tv::ssap("request", "launch", "ssap://system.launcher/launch", payload));
    ok(json!({ "ok": sent, "direct": content.is_some() }))
}

async fn lg_home(State(app): S) -> R {
    // Let the LG home through for 2 minutes, then open it
    app.bypass_until.store(now_ms() + 120_000, Ordering::Relaxed);
    log!("LG home requested, redirect paused 2 min");
    ok(json!({ "ok": app.tv_send(tv::launch_msg(tv::LG_HOME)) }))
}

async fn suggestions(State(app): S) -> R {
    ok(suggest::current(&app).await)
}

async fn rate(State(app): S, text: String) -> R {
    let b = body(&text);
    let kind = if b["type"] == "tv" { "tv" } else { "movie" };
    let verdict = b["verdict"].as_str().unwrap_or("");
    let Some(id) = b["id"].as_i64().filter(|i| *i > 0) else { return bad("bad rating") };
    if !["like", "neutral", "want", "pass", "unseen", "clear"].contains(&verdict) {
        return bad("bad rating");
    }
    let key = suggest::key_of(kind, id);
    {
        let mut ratings = app.ratings.write().await;
        if verdict == "clear" {
            ratings.remove(&key);
        } else {
            let genres = b["genre_ids"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_else(|| ratings.get(&key).map(|r| r.genres.clone()).unwrap_or_default());
            let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
            ratings.insert(key.clone(), Rating { verdict: verdict.into(), title: s("title"), genres, poster: s("poster"), at: now_ms() });
        }
        store::save("ratings.json", &*ratings);
    }
    // Drop it from the current shelf right away, recompute in the background
    if verdict != "unseen" && verdict != "want" {
        app.suggestions.write().await.items.retain(|m| m.key != key);
    }
    app.recompute.notify_one();
    ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct PageQ {
    page: Option<u32>,
}
async fn rate_queue(State(app): S, Query(q): Query<PageQ>) -> R {
    if app.settings.read().await.tmdb_key.is_empty() {
        return ok(json!({ "error": "no-key", "items": [] }));
    }
    let (items, next) = suggest::rate_queue(&app, q.page.unwrap_or(1).max(1)).await?;
    ok(json!({ "items": items, "next": next }))
}

#[derive(Deserialize)]
struct SearchQ {
    q: Option<String>,
    genre: Option<i64>,
    person: Option<i64>,
    page: Option<u32>,
}
async fn search(State(app): S, Query(q): Query<SearchQ>) -> R {
    if app.settings.read().await.tmdb_key.is_empty() {
        return ok(json!({ "error": "no-key", "items": [] }));
    }
    ok(match (q.genre, q.person) {
        (Some(g), _) => suggest::by_genre(&app, g, q.page.unwrap_or(1).clamp(1, 10)).await?,
        (_, Some(p)) => suggest::by_person(&app, p).await?,
        _ => suggest::search(&app, &q.q.unwrap_or_default()).await?,
    })
}

async fn trailer(State(app): S, Query(q): Query<TrailerQ>) -> R {
    let kind = match q.kind.as_deref() { Some("movie") => "movie", Some("tv") => "tv", _ => return bad("bad item") };
    let Some(id) = q.id else { return bad("bad item") };
    // The TV reports a video YouTube won't play in its embedded player and asks for another one
    if let Some(key) = q.blocked.as_deref().filter(|k| !k.is_empty()) { trailers::block(key).await }
    ok(json!({ "trailer": trailers::find(&app, kind, id).await? }))
}

#[derive(Deserialize)]
struct TrailerQ {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<i64>,
    blocked: Option<String>,
}

#[derive(Deserialize)]
struct ItemQ {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<i64>,
}
async fn item(State(app): S, Query(q): Query<ItemQ>) -> R {
    let kind = match q.kind.as_deref() { Some("movie") => "movie", Some("tv") => "tv", _ => return bad("bad item") };
    let Some(id) = q.id.filter(|i| *i > 0) else { return bad("bad item") };
    ok(suggest::details(&app, kind, id).await?)
}

/// Server-sent events: the TV keeps this open and reloads settings / suggestions when told to.
async fn events(State(app): S) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(app.events.subscribe())
        .filter_map(|m| async move { m.ok().map(|name| Ok(Event::default().event(name).data(name))) });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn ratings(State(app): S) -> R {
    let ratings = app.ratings.read().await;
    let mut list: Vec<(&String, &Rating)> = ratings.iter().collect();
    list.sort_by_key(|(_, r)| std::cmp::Reverse(r.at));
    ok(list.into_iter().map(|(k, r)| {
        let (kind, id) = suggest::split_key(k);
        json!({ "key": k, "type": kind, "id": id, "verdict": r.verdict, "title": r.title, "genres": r.genres, "poster": r.poster, "at": r.at })
    }).collect::<Vec<_>>())
}
