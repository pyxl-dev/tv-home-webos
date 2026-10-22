// TNT programme reminders: a TV alert some minutes before the start, with "Ouvrir" (switch to the channel) / "Fermer".
// The alert is an LG system notification, so it shows over any app. "Ouvrir" relaunches the Accueil app with
// { tune: <LG channel id> }, which asks us (/api/tune) to switch channel: the alert can only launch apps.
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::store::{self, now_ms, App};
use crate::{epg, tv};

pub const LEAD_CHOICES: [u32; 4] = [5, 10, 15, 30];
/// Let the TV settle after power-on (and the Accueil launch) before showing an alert
const AFTER_POWER_ON: i64 = 10_000;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    /// "<guide channel id>:<start ms>"
    pub id: String,
    pub guide: String,
    /// LG channel id, empty when the guide channel isn't in the TV's list
    pub channel_id: String,
    pub channel: String,
    pub number: String,
    pub title: String,
    pub start: i64,
    pub stop: i64,
    #[serde(default)]
    pub notified: bool,
}

pub async fn run(app: Arc<App>) {
    loop {
        tick(&app).await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

async fn tick(app: &App) {
    let t = now_ms();
    let lead = app.settings.read().await.reminder_lead as i64 * 60_000;
    let on_since = app.tv_on_since.load(Ordering::Relaxed);
    let ready = on_since > 0 && t - on_since > AFTER_POWER_ON;
    let mut list = app.reminders.lock().await;
    let before = list.len();
    list.retain(|r| r.stop > t);
    let mut changed = list.len() != before;
    // TV off at the time: the alert waits until it's turned on, as long as the programme isn't over
    for r in list.iter_mut().filter(|r| !r.notified && t >= r.start - lead) {
        if ready && app.tv_send(alert(r, t)) {
            log!("reminder: {} on {}", r.title, r.channel);
            r.notified = true;
            changed = true;
        }
    }
    if changed {
        store::save("reminders.json", &*list);
        let _ = app.events.send("reminders");
    }
}

fn alert(r: &Reminder, t: i64) -> String {
    let mins = (r.start - t + 30_000) / 60_000;
    let when = match mins {
        m if m >= 1 => format!("commence dans {m} min"),
        _ if t < r.start => "commence maintenant".to_string(),
        _ => format!("a commencé il y a {} min", (t - r.start) / 60_000),
    };
    let open = if r.channel_id.is_empty() {
        json!({ "id": tv::LIVE_APP })
    } else {
        json!({ "id": tv::HOME_APP, "params": { "tune": r.channel_id, "at": t } })
    };
    let payload = json!({
        "message": format!("⏰ « {} » {when} sur {}", r.title, r.channel),
        "buttons": [
            { "label": "Ouvrir", "focus": true, "onclick": "luna://com.webos.applicationManager/launch", "params": open },
            { "label": "Fermer" }
        ]
    });
    tv::ssap("request", "alert", "ssap://system.notifications/createAlert", payload)
}

/// Add a reminder for the programme starting at `start` on guide channel `guide`.
pub async fn add(app: &App, guide: &str, start: i64) -> Option<Reminder> {
    let (prog, guide_name) = epg::find(app, guide, start).await?;
    let ch = {
        let channels = app.state.lock().await.channels.clone();
        let epg = app.epg.read().await;
        channels.into_iter().find(|c| epg.channel_id(&c.name).as_deref() == Some(guide))
    };
    let r = Reminder {
        id: format!("{guide}:{start}"),
        guide: guide.into(),
        channel_id: ch.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
        channel: ch.as_ref().map(|c| c.name.clone()).unwrap_or(guide_name),
        number: ch.map(|c| c.number).unwrap_or_default(),
        title: prog.title,
        start: prog.start,
        stop: prog.stop,
        notified: false,
    };
    let mut list = app.reminders.lock().await;
    list.retain(|x| x.id != r.id);
    list.push(r.clone());
    list.sort_by_key(|x| x.start);
    store::save("reminders.json", &*list);
    let _ = app.events.send("reminders");
    Some(r)
}

pub async fn remove(app: &App, id: &str) {
    let mut list = app.reminders.lock().await;
    list.retain(|x| x.id != id);
    store::save("reminders.json", &*list);
    let _ = app.events.send("reminders");
}
