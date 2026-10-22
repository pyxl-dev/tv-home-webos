// Connection to the TV over LG's network remote API (SSAP, wss://<tv>:3001), already paired.
// One session = one websocket; the loop reconnects every 5 s when the TV is off or silent.
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep_until, Instant};
use tokio_tungstenite::{tungstenite::Message, Connector};

use crate::store::{self, now_ms, App, Channel};

pub const HOME_APP: &str = "com.yoan.home";
pub const LIVE_APP: &str = "com.webos.app.livetv";
pub const LG_HOME: &str = "com.webos.app.home";

const LAUNCH_DELAY: Duration = Duration::from_secs(4); // let webOS finish booting its own home first
const SETTLE: Duration = Duration::from_millis(700); // LG home animates in and grabs focus back
const BEAT: Duration = Duration::from_secs(20);
const SILENT: Duration = Duration::from_secs(45);

pub struct Config { ip: String, mac: Option<[u8; 6]> }

/// TV address (TVHOME_TV_IP) and MAC for Wake-on-LAN (TVHOME_TV_MAC, optional), see tv-home.env.example.
pub fn config() -> anyhow::Result<&'static Config> {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    if let Some(c) = CONFIG.get() { return Ok(c); }
    let ip = std::env::var("TVHOME_TV_IP").map_err(|_| anyhow::anyhow!("TVHOME_TV_IP is not set (see tv-home.env.example)"))?;
    let mac = match std::env::var("TVHOME_TV_MAC") {
        Ok(s) => Some(parse_mac(&s).ok_or_else(|| anyhow::anyhow!("TVHOME_TV_MAC must look like aa:bb:cc:dd:ee:ff"))?),
        Err(_) => None,
    };
    Ok(CONFIG.get_or_init(|| Config { ip, mac }))
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let bytes: Option<Vec<u8>> = s.trim().split([':', '-']).map(|b| u8::from_str_radix(b, 16).ok()).collect();
    bytes?.try_into().ok()
}

fn tv_url() -> anyhow::Result<String> {
    Ok(format!("wss://{}:3001", config()?.ip))
}

pub fn ssap(kind: &str, id: &str, uri: &str, payload: Value) -> String {
    json!({ "type": kind, "id": id, "uri": uri, "payload": payload }).to_string()
}

pub fn launch_msg(app_id: &str) -> String {
    ssap("request", "launch", "ssap://system.launcher/launch", json!({ "id": app_id }))
}

/// Wake-on-LAN magic packet; only works when "Turn on via Wi-Fi" is enabled on the TV.
pub fn wake() -> bool {
    let Ok(Config { ip, mac: Some(mac) }) = config() else { return false };
    let mut packet = vec![0xffu8; 6];
    for _ in 0..16 { packet.extend_from_slice(mac); }
    let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else { return false };
    if sock.set_broadcast(true).is_err() { return false; }
    // Subnet broadcast assumes a /24 network, like most home routers
    let subnet = ip.rsplit_once('.').map(|(net, _)| format!("{net}.255:9"));
    [subnet, Some("255.255.255.255:9".into()), Some(format!("{ip}:9"))].iter().flatten()
        .filter(|a| sock.send_to(&packet, a.as_str()).is_ok()).count() > 0
}

/// Capture of what the TV shows (960×540 JPEG, video included; DRM apps may come out black).
/// Captures are serialized and a frame under 200 ms old is reused, so several phones can watch at once.
pub async fn screen(app: &App) -> anyhow::Result<Vec<u8>> {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    let mut last = app.screen.lock().await;
    if let Some((at, jpg)) = last.as_ref() {
        if at.elapsed() < Duration::from_millis(200) { return Ok(jpg.clone()); }
    }
    let (tx, rx) = oneshot::channel();
    *app.shot_reply.lock().unwrap() = Some(tx);
    if !app.tv_send(ssap("request", "shot", "ssap://tv/executeOneShot", json!({}))) { anyhow::bail!("tv unreachable"); }
    let uri = tokio::time::timeout(Duration::from_secs(3), rx).await
        .map_err(|_| anyhow::anyhow!("capture timed out"))?
        .map_err(|_| anyhow::anyhow!("tv disconnected"))?
        .map_err(|e| anyhow::anyhow!(e))?;
    // The TV serves it over https with its self-signed certificate
    let http = HTTP.get_or_init(|| reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(3)).build().expect("http client"));
    let jpg = http.get(&uri).send().await?.error_for_status()?.bytes().await?.to_vec();
    *last = Some((std::time::Instant::now(), jpg.clone()));
    Ok(jpg)
}

/// Live status changed: tell the phone page.
fn live_changed(app: &App, f: impl FnOnce(&mut crate::store::TvLive)) {
    f(&mut app.tv_live.lock().unwrap());
    let _ = app.events.send("tv");
}

fn manifest() -> Value {
    serde_json::from_str(&std::fs::read_to_string("manifest.json").expect("manifest.json")).expect("manifest")
}

async fn connect(url: &str) -> anyhow::Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    let tls = native_tls::TlsConnector::builder().danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true).build()?;
    let (ws, _) = tokio::time::timeout(Duration::from_secs(4), tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(Connector::NativeTls(tls))))
        .await
        .map_err(|_| anyhow::anyhow!("connect timed out"))??;
    Ok(ws)
}

/// A remote button (UP, DOWN, LEFT, RIGHT, ENTER, BACK, HOME…) through the pointer socket; false if the TV didn't grant it.
pub fn button(app: &App, name: &str) -> bool {
    match app.pointer_tx.lock().unwrap().as_ref() {
        Some(tx) => tx.send(format!("type:button\nname:{name}\n\n")).is_ok(),
        None => false,
    }
}

/// Second websocket the TV opens for pointer and button input; lives as long as the TV keeps it.
async fn pointer(app: Arc<App>, url: String) {
    let ws = match connect(&url).await {
        Ok(ws) => ws,
        Err(e) => { log!("pointer socket: {e}"); return; }
    };
    log!("pointer socket open: arrows and OK work on every app");
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    *app.pointer_tx.lock().unwrap() = Some(tx);
    let _ = app.events.send("tv");
    loop {
        tokio::select! {
            Some(out) = rx.recv() => if sink.send(Message::Text(out)).await.is_err() { break },
            msg = stream.next() => match msg { Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break, _ => {} },
        }
    }
    *app.pointer_tx.lock().unwrap() = None;
    let _ = app.events.send("tv");
}

/// New pairing (asked from the phone): the TV shows its prompt, someone accepts it with the real remote.
/// The new key replaces the old one only if the TV grants the pointer socket with it (the point of re-pairing).
pub async fn pair(app: Arc<App>) {
    let set = |s: &'static str| { *app.pair_state.lock().unwrap() = s; let _ = app.events.send("tv"); };
    set("waiting");
    let attempt = async {
        let ws = connect(&tv_url()?).await?;
        let (mut sink, mut stream) = ws.split();
        let register = json!({ "type": "register", "id": "reg", "payload": { "pairingType": "PROMPT", "manifest": manifest() } });
        sink.send(Message::Text(register.to_string())).await?;
        let mut key = String::new();
        while let Some(msg) = stream.next().await {
            let Message::Text(text) = msg? else { continue };
            let r: Value = serde_json::from_str(&text).unwrap_or_default();
            match (r["type"].as_str().unwrap_or(""), r["id"].as_str().unwrap_or("")) {
                ("registered", _) => {
                    key = r["payload"]["client-key"].as_str().unwrap_or("").to_string();
                    sink.send(Message::Text(ssap("request", "ptr", "ssap://com.webos.service.networkinput/getPointerInputSocket", json!({})))).await?;
                }
                (_, "ptr") => return Ok::<_, anyhow::Error>((key, r["payload"]["socketPath"].is_string())),
                ("error", _) => anyhow::bail!(r["error"].as_str().unwrap_or("pairing refused").to_string()),
                _ => {}
            }
        }
        anyhow::bail!("tv closed the connection")
    };
    match tokio::time::timeout(Duration::from_secs(75), attempt).await {
        Ok(Ok((key, true))) if !key.is_empty() => {
            let _ = std::fs::copy("client-key.txt", "client-key.old.txt");
            match std::fs::write("client-key.txt", &key) {
                Ok(()) => { log!("paired again: pointer socket granted"); set("ok"); app.tv_reset.notify_one(); }
                Err(e) => { log!("pairing: can't save key: {e}"); set("failed"); }
            }
        }
        Ok(Ok(_)) => { log!("paired again but pointer socket still refused: old key kept"); set("refused"); }
        Ok(Err(e)) => { log!("pairing failed: {e}"); set("failed"); }
        Err(_) => { log!("pairing: nobody accepted on the TV"); set("failed"); }
    }
}

pub async fn run(app: Arc<App>) {
    let manifest = manifest();
    loop {
        // Read each time: a new pairing from the phone may have replaced it
        let key = std::fs::read_to_string("client-key.txt").expect("client-key.txt").trim().to_string();
        if let Err(e) = session(&app, &manifest, &key).await {
            // TV off or unreachable most of the time: stay quiet unless something else broke
            let msg = e.to_string();
            if !msg.contains("timed out") && !msg.contains("refused") && !msg.contains("unreachable") && !msg.contains("Host is down") {
                log!("tv session ended: {msg}");
            }
        }
        *app.tv_tx.lock().unwrap() = None;
        *app.pointer_tx.lock().unwrap() = None;
        if app.tv_on_since.swap(0, Ordering::Relaxed) != 0 { let _ = app.events.send("tv"); }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn session(app: &Arc<App>, manifest: &Value, key: &str) -> anyhow::Result<()> {
    let ws = connect(&tv_url()?).await?;
    let (mut sink, mut stream) = ws.split();
    let register = json!({ "type": "register", "id": "reg",
        "payload": { "pairingType": "PROMPT", "client-key": key, "manifest": manifest } });
    sink.send(Message::Text(register.to_string())).await?;

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut launched = false; // home already opened for this power-on
    let mut launch_at: Option<Instant> = None;
    let mut power_known = false;
    let mut power_check_at: Option<Instant> = None;
    let mut chan_at: Option<Instant> = None;
    let mut redirect_at: Option<Instant> = None;
    let mut redir_start = Instant::now() - Duration::from_secs(60);
    let mut redir_tries = 0;
    let mut current_app = String::new();
    let mut last_answer = Instant::now();
    let mut beat = tokio::time::interval(BEAT);
    beat.reset();
    let far = || Instant::now() + Duration::from_secs(86400);

    loop {
        tokio::select! {
            msg = stream.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(_)) => continue,
                };
                last_answer = Instant::now();
                let r: Value = match serde_json::from_str(&text) { Ok(v) => v, Err(_) => continue };
                let p = &r["payload"];
                let id = r["id"].as_str().unwrap_or("");
                match (r["type"].as_str().unwrap_or(""), id) {
                    ("registered", _) => {
                        log!("tv connected");
                        *app.tv_tx.lock().unwrap() = Some(tx.clone());
                        // TV already on when the companion (re)started: don't interrupt whatever is playing
                        if app.started.elapsed() < Duration::from_secs(10) { launched = true; }
                        for (kind, id, uri) in [
                            ("subscribe", "power", "ssap://com.webos.service.tvpower/power/getPowerState"),
                            ("subscribe", "fg", "ssap://com.webos.applicationManager/getForegroundAppInfo"),
                            ("subscribe", "vol", "ssap://audio/getVolume"),
                            ("request", "chlist", "ssap://tv/getChannelList"),
                            ("request", "ptr", "ssap://com.webos.service.networkinput/getPointerInputSocket"),
                        ] {
                            sink.send(Message::Text(ssap(kind, id, uri, json!({})))).await?;
                        }
                        // If power state never arrives, the connection itself means the TV is on
                        power_check_at = Some(Instant::now() + Duration::from_millis(1500));
                        beat.reset();
                    }
                    (kind, "shot") => {
                        let reply = if kind == "error" { Err(r["error"].as_str().unwrap_or("capture refused").to_string()) }
                            else { p["imageUri"].as_str().map(String::from).ok_or_else(|| "no image".to_string()) };
                        if let Some(w) = app.shot_reply.lock().unwrap().take() { let _ = w.send(reply); }
                    }
                    (kind, "ptr") => match p["socketPath"].as_str() {
                        Some(url) if kind != "error" => { tokio::spawn(pointer(app.clone(), url.to_string())); }
                        // Usual answer with this pairing: arrows / OK then only reach our own app (relayed over SSE)
                        _ => log!("pointer socket refused: {}", r["error"].as_str().unwrap_or("no socket")),
                    },
                    ("error", _) => {}
                    (_, "vol") => {
                        // webOS 5+ nests it in volumeStatus
                        let v = if p["volumeStatus"].is_object() { &p["volumeStatus"] } else { p };
                        let (volume, muted) = (v["volume"].as_i64(), v["muteStatus"].as_bool().or(v["muted"].as_bool()).unwrap_or(false));
                        if volume.is_some() { live_changed(app, |l| { l.volume = volume; l.muted = muted; }); }
                    }
                    (_, "power") => {
                        power_known = true;
                        let state = p["state"].as_str().unwrap_or("");
                        log!("power {state}");
                        if state == "Active" && p["processing"].is_null() {
                            if app.tv_on_since.compare_exchange(0, now_ms(), Ordering::Relaxed, Ordering::Relaxed).is_ok() { let _ = app.events.send("tv"); }
                            if !launched && launch_at.is_none() { launch_at = Some(Instant::now() + LAUNCH_DELAY); }
                        } else {
                            if app.tv_on_since.swap(0, Ordering::Relaxed) != 0 { let _ = app.events.send("tv"); }
                            launched = false;
                            launch_at = None;
                        }
                    }
                    (_, "fg") => {
                        let Some(fg) = p["appId"].as_str() else { continue };
                        log!("app {}", if fg.is_empty() { "(none)" } else { fg });
                        if !fg.is_empty() && fg != HOME_APP {
                            let mut st = app.state.lock().await;
                            st.seen_apps.insert(fg.to_string(), now_ms());
                            store::save("state.json", &*st);
                        }
                        current_app = fg.to_string();
                        live_changed(app, |l| l.app = fg.to_string());
                        redirect_at = None;
                        // Home button opens the LG home: swap it for ours once it has settled (retry up to 3×)
                        if fg == LG_HOME && app.settings.read().await.home_redirect {
                            if now_ms() <= app.bypass_until.load(Ordering::Relaxed) {
                                log!("home button: LG home allowed (bypass)");
                            } else {
                                if redir_start.elapsed() > Duration::from_secs(8) { redir_start = Instant::now(); redir_tries = 0; }
                                redirect_at = Some(Instant::now() + SETTLE);
                            }
                        }
                        // Poll the channel while live TV is on screen (only then does the TV tell us)
                        if fg == LIVE_APP { if chan_at.is_none() { chan_at = Some(Instant::now() + Duration::from_millis(1500)); } }
                        else { chan_at = None; }
                    }
                    (_, "chan") if p["returnValue"].as_bool() == Some(true) => {
                        let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
                        let ch = Channel { number: s("channelNumber"), name: s("channelName"), id: s("channelId") };
                        let mut st = app.state.lock().await;
                        if st.last_channel.as_ref() != Some(&ch) {
                            log!("channel {} {}", ch.number, ch.name);
                            st.last_channel = Some(ch);
                            store::save("state.json", &*st);
                            let _ = app.events.send("tv");
                        }
                    }
                    (_, "chlist") => {
                        if let Some(list) = p["channelList"].as_array() {
                            let s = |c: &Value, k: &str| c[k].as_str().unwrap_or("").to_string();
                            let mut st = app.state.lock().await;
                            st.channels = list.iter().map(|c| Channel { number: s(c, "channelNumber"), name: s(c, "channelName"), id: s(c, "channelId") }).collect();
                            store::save("state.json", &*st);
                        }
                    }
                    _ => {}
                }
            }
            Some(out) = rx.recv() => sink.send(Message::Text(out)).await?,
            _ = app.tv_reset.notified() => { log!("reconnecting with the new client key"); return Ok(()); }
            _ = beat.tick() => {
                // The TV can drop us without closing the socket (network standby), silently killing subscriptions
                if last_answer.elapsed() > SILENT { log!("tv silent, reconnecting"); return Ok(()); }
                sink.send(Message::Text(ssap("request", "beat", "ssap://com.webos.applicationManager/getForegroundAppInfo", json!({})))).await?;
            }
            _ = sleep_until(launch_at.unwrap_or_else(far)), if launch_at.is_some() => {
                launch_at = None;
                launched = true;
                log!("launch home");
                sink.send(Message::Text(launch_msg(HOME_APP))).await?;
            }
            _ = sleep_until(power_check_at.unwrap_or_else(far)), if power_check_at.is_some() => {
                power_check_at = None;
                if !power_known {
                    if app.tv_on_since.compare_exchange(0, now_ms(), Ordering::Relaxed, Ordering::Relaxed).is_ok() { let _ = app.events.send("tv"); }
                    if !launched && launch_at.is_none() { launch_at = Some(Instant::now() + LAUNCH_DELAY); }
                }
            }
            _ = sleep_until(chan_at.unwrap_or_else(far)), if chan_at.is_some() => {
                chan_at = Some(Instant::now() + Duration::from_secs(15));
                sink.send(Message::Text(ssap("request", "chan", "ssap://tv/getCurrentChannel", json!({})))).await?;
            }
            _ = sleep_until(redirect_at.unwrap_or_else(far)), if redirect_at.is_some() => {
                redirect_at = None;
                if current_app == LG_HOME && redir_tries < 3 {
                    redir_tries += 1;
                    log!("home button -> Accueil (try {redir_tries})");
                    sink.send(Message::Text(launch_msg(HOME_APP))).await?;
                }
            }
        }
    }
}
