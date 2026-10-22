// TV Home companion — runs on the Mac, always on.
// - Opens the "Accueil" app whenever the TV turns on, and optionally instead of the LG home
//   (LG SSAP remote API, already paired)
// - Remembers the last live channel and which apps get opened (for the "Ajouter" picker)
// - Shows reminders for TNT programmes on the TV, whatever app is open
// - Serves the TV guide (xmltvfr.fr), the home app list and film/series suggestions (TMDB)
// - Serves the phone page to rate titles: http://<mac-ip>:8787
//
// Run from the companion directory: it reads `data/`, `public/`, `manifest.json`, `client-key.txt`.

macro_rules! log {
    ($($t:tt)*) => {
        println!("{} {}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"), format!($($t)*))
    };
}

mod epg;
mod http;
mod justwatch;
mod providers;
mod reminders;
mod store;
mod suggest;
mod theme;
mod tmdb;
mod trailer;
mod tv;

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = Arc::new(store::App::load()?);
    // TVHOME_NO_TV=1 runs without talking to the TV (testing on a copy of data/, with TVHOME_PORT)
    if std::env::var_os("TVHOME_NO_TV").is_none() {
        tv::config()?; // stop here with a clear message when TVHOME_TV_IP is missing
        tokio::spawn(tv::run(app.clone()));
    }
    tokio::spawn(epg::run(app.clone()));
    tokio::spawn(suggest::run(app.clone()));
    tokio::spawn(reminders::run(app.clone()));
    http::serve(app).await
}
