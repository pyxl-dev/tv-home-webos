// Film + series suggestions from the user's ratings, filtered by their streaming services.
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::providers::{self, Provider};
use crate::store::{now_ms, App, Item, PublicProvider, Rating, SeasonTag, Suggestions, Watch};
use crate::tmdb::{DAY, H};

pub const TYPES: [&str; 2] = ["movie", "tv"];
const IMG: &str = "https://image.tmdb.org/t/p/";
const STALE_MS: i64 = 6 * 3_600_000;

pub fn key_of(kind: &str, id: i64) -> String {
    format!("{kind}:{id}")
}

/// "tv:123" -> ("tv", 123)
pub fn split_key(key: &str) -> (&'static str, i64) {
    let (kind, id) = key.split_once(':').unwrap_or(("movie", key));
    (if kind == "tv" { "tv" } else { "movie" }, id.parse().unwrap_or(0))
}

pub async fn tmdb(app: &App, q: &str, ttl: Duration) -> anyhow::Result<Arc<Value>> {
    let key = app.settings.read().await.tmdb_key.clone();
    app.tmdb.get(&key, q, ttl).await
}

fn results(v: &Value) -> Vec<Value> {
    v["results"].as_array().cloned().unwrap_or_default()
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

pub async fn load_genres(app: &App) -> anyhow::Result<()> {
    if !app.genres.read().await.is_empty() {
        return Ok(());
    }
    let mut names = HashMap::new();
    for kind in TYPES {
        let g = tmdb(app, &format!("/genre/{kind}/list?language=fr-FR"), 7 * DAY).await?;
        for x in g["genres"].as_array().into_iter().flatten() {
            if let Some(id) = x["id"].as_i64() {
                names.insert(id, text(x, "name"));
            }
        }
    }
    *app.genres.write().await = names;
    Ok(())
}

// Series genres mapped onto film genres so one taste profile covers both
fn taste_genres(ids: &[i64]) -> Vec<i64> {
    ids.iter()
        .flat_map(|g| match g {
            10759 => vec![28, 12],
            10765 => vec![878, 14],
            10768 => vec![10752],
            10762 => vec![10751],
            10763 => vec![99],
            10766 => vec![18],
            g => vec![*g],
        })
        .collect()
}

fn genre_ids(m: &Value) -> Vec<i64> {
    match m["genre_ids"].as_array() {
        Some(a) => a.iter().filter_map(Value::as_i64).collect(),
        None => m["genres"].as_array().map(|a| a.iter().filter_map(|g| g["id"].as_i64()).collect()).unwrap_or_default(),
    }
}

pub fn shape(m: &Value, kind: &str, provider: Option<PublicProvider>, genres: &HashMap<i64, String>) -> Item {
    let id = m["id"].as_i64().unwrap_or(0);
    let ids = genre_ids(m);
    let date = m["release_date"].as_str().or(m["first_air_date"].as_str()).unwrap_or("");
    let img = |k: &str, size: &str| m[k].as_str().map(|p| format!("{IMG}{size}{p}")).unwrap_or_default();
    Item {
        key: key_of(kind, id),
        kind: kind.into(),
        id,
        title: m["title"].as_str().or(m["name"].as_str()).unwrap_or("").into(),
        year: date.chars().take(4).collect(),
        genres: ids.iter().filter_map(|g| genres.get(g).cloned()).take(2).collect(),
        genre_ids: ids,
        overview: text(m, "overview"),
        vote: (m["vote_average"].as_f64().unwrap_or(0.0) * 10.0).round() / 10.0,
        backdrop: img("backdrop_path", "w1280"),
        poster: img("poster_path", "w342"),
        provider,
        rating: None,
        length: None,
        watch: None,
        season: None,
        rewatch: false,
    }
}

/// Best service for a title = the first enabled one, in the user's priority order.
async fn provider_for(app: &App, region: &str, enabled: &[&'static Provider], seen: &HashMap<String, i64>, kind: &str, id: i64) -> Option<PublicProvider> {
    let r = tmdb(app, &format!("/{kind}/{id}/watch/providers"), 2 * DAY).await.ok()?;
    let flat: Vec<i64> = r["results"][region]["flatrate"].as_array()?.iter().filter_map(|x| x["provider_id"].as_i64()).collect();
    let on: Vec<&&Provider> = enabled.iter().filter(|p| p.ids.iter().any(|i| flat.contains(i))).collect();
    let mut best = providers::public(on.first()?, seen);
    best.also = on[1..].iter().map(|p| p.name.to_string()).collect();
    Some(best)
}

/// A themed period of the year: its picks get a label on the TV and a couple of slots in every batch.
pub struct Season {
    pub key: &'static str,
    pub label: &'static str,
    pub color: &'static str,
    /// (month, day) inclusive
    from: (u32, u32),
    to: (u32, u32),
    /// Extra /discover filters; no series filter = films only
    movie: &'static str,
    tv: Option<&'static str>,
}

const SEASONS: [Season; 3] = [
    Season { key: "halloween", label: "Spécial Halloween", color: "#ff7a1a", from: (10, 1), to: (11, 2),
             movie: "&with_genres=27", tv: Some("&with_keywords=315058|3335") },
    Season { key: "noel", label: "Spécial Noël", color: "#ff4d4d", from: (12, 1), to: (12, 26),
             movie: "&with_keywords=207317", tv: Some("&with_keywords=207317") },
    Season { key: "valentin", label: "Spécial Saint-Valentin", color: "#ff5c8a", from: (2, 1), to: (2, 14),
             movie: "&with_genres=10749", tv: None },
];

pub fn season_now() -> Option<&'static Season> {
    use chrono::Datelike;
    let d = chrono::Local::now().date_naive();
    let md = (d.month(), d.day());
    SEASONS.iter().find(|s| s.from <= md && md <= s.to)
}

/// Seasonal picks on the user's services, ranked by popularity, rating and taste.
async fn seasonal(app: &App, season: &Season, ctx: &Ctx<'_>, taken: &mut std::collections::HashSet<String>) -> Vec<Item> {
    let tag = SeasonTag { key: season.key.into(), label: season.label.into(), color: season.color.into() };
    let mut out = Vec::new();
    for (kind, filter, quota) in [("movie", Some(season.movie), 8), ("tv", season.tv, 4)] {
        let Some(filter) = filter else { continue };
        let mut cand: HashMap<String, Cand> = HashMap::new();
        for page in 1..=3 {
            let q = format!(
                "/discover/{kind}?language=fr-FR&watch_region={}&with_watch_providers={}&with_watch_monetization_types=flatrate&sort_by=popularity.desc&vote_count.gte={}{filter}&page={page}",
                ctx.region, ctx.avail, if kind == "tv" { 100 } else { 300 });
            if let Ok(r) = tmdb(app, &q, 6 * H).await {
                for (i, m) in results(&r).iter().enumerate() {
                    add(&mut cand, ctx.ratings, m, kind, 1.0 - ((page - 1) * 20 + i as u32) as f64 / 60.0);
                }
            }
        }
        let mut ranked: Vec<Cand> = cand.into_iter().filter(|(k, _)| !taken.contains(k)).map(|(_, c)| c).collect();
        for c in ranked.iter_mut() {
            c.score += ctx.taste_score(&c.m) + (c.m["vote_average"].as_f64().unwrap_or(6.5) - 6.5) * 0.3;
        }
        ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
        ranked.truncate(30);
        let provs = join_all(ranked.iter().map(|c| provider_for(app, ctx.region, ctx.enabled, ctx.seen, kind, c.m["id"].as_i64().unwrap_or(0)))).await;
        for (c, p) in ranked.iter().zip(provs).filter_map(|(c, p)| Some((c, p?))).take(quota) {
            let mut it = shape(&c.m, kind, Some(p), ctx.genres);
            it.season = Some(tag.clone());
            taken.insert(it.key.clone());
            out.push(it);
        }
    }
    out
}

struct Ctx<'a> {
    region: &'a str,
    avail: String,
    enabled: &'a [&'static Provider],
    seen: &'a HashMap<String, i64>,
    ratings: &'a HashMap<String, Rating>,
    genres: &'a HashMap<i64, String>,
    taste: &'a HashMap<i64, f64>,
    taste_max: f64,
}

impl Ctx<'_> {
    fn taste_score(&self, m: &Value) -> f64 {
        let gs = taste_genres(&genre_ids(m));
        if gs.is_empty() {
            return 0.0;
        }
        0.8 * gs.iter().map(|g| self.taste.get(g).copied().unwrap_or(0.0)).sum::<f64>() / gs.len() as f64 / self.taste_max
    }
}

struct Cand {
    m: Value,
    kind: &'static str,
    score: f64,
}

fn add(cand: &mut HashMap<String, Cand>, ratings: &HashMap<String, Rating>, m: &Value, kind: &'static str, w: f64) {
    let Some(id) = m["id"].as_i64() else { return };
    let k = key_of(kind, id);
    let rated_out = ratings.get(&k).is_some_and(|r| r.verdict != "unseen" && r.verdict != "want");
    if m["backdrop_path"].as_str().is_none() || rated_out {
        return;
    }
    cand.entry(k).or_insert_with(|| Cand { m: m.clone(), kind, score: 0.0 }).score += w;
}

async fn compute(app: &App) -> anyhow::Result<Suggestions> {
    let settings = app.settings.read().await.clone();
    if settings.tmdb_key.is_empty() {
        return Ok(Suggestions { at: now_ms(), items: vec![], error: Some("no-key".into()) });
    }
    let enabled = providers::enabled(&settings);
    if enabled.is_empty() {
        return Ok(Suggestions { at: now_ms(), items: vec![], error: Some("no-provider".into()) });
    }
    load_genres(app).await?;
    let genres = app.genres.read().await.clone();
    let ratings = app.ratings.read().await.clone();
    let seen = app.state.lock().await.seen_apps.clone();

    let by = |v: &str| -> Vec<(String, Rating)> {
        let mut l: Vec<_> = ratings.iter().filter(|(_, r)| r.verdict == v).map(|(k, r)| (k.clone(), r.clone())).collect();
        l.sort_by_key(|(_, r)| std::cmp::Reverse(r.at));
        l
    };
    let mut likes = by("like");
    let all_likes = likes.clone();
    likes.truncate(30);
    let (passes, wants) = (by("pass"), by("want"));

    // Genre taste: liked genres up, passed genres down, "Intéressé" a little up.
    // "Neutre" (seen, no strong opinion) leaves taste alone; like any seen title it's never suggested.
    let mut taste: HashMap<i64, f64> = HashMap::new();
    for (list, w) in [(&likes, 1.0), (&passes, -0.7), (&wants, 0.5)] {
        for (_, r) in list {
            for g in taste_genres(&r.genres) {
                *taste.entry(g).or_default() += w;
            }
        }
    }
    let taste_max = taste.values().fold(1.0f64, |m, v| m.max(v.abs()));

    let mut cand: HashMap<String, Cand> = HashMap::new();
    let rank_w = |i: usize| 1.0 + (20.0 - i as f64) / 20.0;
    for (k, _) in &likes {
        let (kind, id) = split_key(k);
        if let Ok(r) = tmdb(app, &format!("/{kind}/{id}/recommendations?language=fr-FR"), 6 * H).await {
            for (i, m) in results(&r).iter().enumerate() {
                add(&mut cand, &ratings, m, kind, rank_w(i));
            }
        }
    }
    // Titles marked "Intéressé" go straight to the top, and their neighbours count a bit too
    for (k, _) in &wants {
        let (kind, id) = split_key(k);
        if let Ok(d) = tmdb(app, &format!("/{kind}/{id}?language=fr-FR"), 2 * DAY).await {
            add(&mut cand, &ratings, &d, kind, 4.0);
        }
        if let Ok(r) = tmdb(app, &format!("/{kind}/{id}/recommendations?language=fr-FR"), 6 * H).await {
            for (i, m) in results(&r).iter().enumerate() {
                add(&mut cand, &ratings, m, kind, 0.5 * rank_w(i));
            }
        }
    }
    let avail: Vec<String> = enabled.iter().flat_map(|p| p.ids.iter().map(|i| i.to_string())).collect();
    let ctx = Ctx { region: &settings.region, avail: avail.join("|"), enabled: &enabled, seen: &seen,
                    ratings: &ratings, genres: &genres, taste: &taste, taste_max };
    for kind in TYPES {
        for page in 1..=3 {
            let q = format!(
                "/discover/{kind}?language=fr-FR&watch_region={}&with_watch_providers={}&with_watch_monetization_types=flatrate&sort_by=popularity.desc&vote_count.gte={}&page={page}",
                ctx.region, ctx.avail, if kind == "tv" { 150 } else { 300 });
            if let Ok(r) = tmdb(app, &q, 6 * H).await {
                for m in results(&r) {
                    add(&mut cand, &ratings, &m, kind, 0.4);
                }
            }
        }
    }
    for (k, c) in cand.iter_mut() {
        c.score += ctx.taste_score(&c.m);
        c.score += (c.m["vote_average"].as_f64().unwrap_or(6.5) - 6.5) * 0.15;
        if ratings.get(k).is_some_and(|r| r.verdict == "unseen") {
            c.score += 0.2;
        }
    }

    // Pool of up to 40 available titles: films and series are ranked separately so neither
    // drowns the other; the TV shows a rotating selection of them
    let mut by_type: Vec<VecDeque<Item>> = Vec::new();
    let mut off: Vec<Item> = Vec::new();
    for (kind, quota) in [("movie", 24), ("tv", 16)] {
        let mut ranked: Vec<&Cand> = cand.values().filter(|c| c.kind == kind).collect();
        ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
        ranked.truncate(120);
        let mut picked = VecDeque::new();
        for batch in ranked.chunks(10) {
            if picked.len() >= quota {
                break;
            }
            let provs = join_all(batch.iter().map(|c| provider_for(app, &settings.region, &enabled, &seen, c.kind, c.m["id"].as_i64().unwrap_or(0)))).await;
            for (c, p) in batch.iter().zip(provs) {
                match p {
                    Some(p) if picked.len() < quota => picked.push_back(shape(&c.m, c.kind, Some(p), &genres)),
                    // Strong matches (recommended from likes, not just popular) that no service carries
                    None if settings.off_platform && c.score > 1.5 && off.len() < 8 => off.push(shape(&c.m, c.kind, None, &genres)),
                    _ => {}
                }
            }
        }
        by_type.push(picked);
    }
    log!("suggestions {} films + {} series + {} off-platform from {} candidates, {} likes", by_type[0].len(), by_type[1].len(), off.len(), cand.len(), likes.len());

    // Interleave (~3 films : 2 series) so both appear in every batch
    let mut items = Vec::new();
    let mut i = 0;
    while !by_type[0].is_empty() || !by_type[1].is_empty() {
        items.extend(by_type[0].pop_front());
        if i % 3 != 2 {
            items.extend(by_type[1].pop_front());
        }
        i += 1;
    }

    // Off-platform and seasonal picks go at the end of the pool; the TV gives each a couple of slots per batch
    items.extend(off);
    // Seasonal picks go at the end of the pool; the TV gives them a couple of slots in every batch
    if let Some(season) = season_now().filter(|_| settings.seasonal) {
        let mut taken: std::collections::HashSet<String> = items.iter().map(|it| it.key.clone()).collect();
        let extra = seasonal(app, season, &ctx, &mut taken).await;
        log!("suggestions {} {} picks", extra.len(), season.key);
        items.extend(extra);
    }
    let again = rewatch(app, &ctx, &all_likes).await;
    log!("suggestions {} to watch again", again.len());
    items.extend(again);
    Ok(Suggestions { at: now_ms(), items, error: None })
}

/// Liked titles still on the user's services, longest-liked first: the TV slips one into each batch.
async fn rewatch(app: &App, ctx: &Ctx<'_>, likes: &[(String, Rating)]) -> Vec<Item> {
    let mut out = Vec::new();
    for chunk in likes.iter().rev().collect::<Vec<_>>().chunks(10) {
        if out.len() >= 8 {
            break;
        }
        let found = join_all(chunk.iter().map(|(k, _)| async move {
            let (kind, id) = split_key(k);
            let p = provider_for(app, ctx.region, ctx.enabled, ctx.seen, kind, id).await?;
            let d = tmdb(app, &format!("/{kind}/{id}?language=fr-FR"), 2 * DAY).await.ok()?;
            d["backdrop_path"].is_string().then(|| shape(&d, kind, Some(p), ctx.genres))
        })).await;
        for mut it in found.into_iter().flatten().take(8 - out.len()) {
            it.rewatch = true;
            out.push(it);
        }
    }
    out
}

/// Recompute now (one computation at a time).
pub async fn refresh(app: &App) {
    let _guard = app.compute_lock.lock().await;
    match compute(app).await {
        Ok(s) => {
            *app.suggestions.write().await = s;
            let _ = app.events.send("suggestions");
        }
        Err(e) => {
            log!("suggestions error: {e}");
            let mut s = app.suggestions.write().await;
            s.at = now_ms();
            s.error = Some(e.to_string());
        }
    }
}

/// Fresh suggestions for the API: recompute first when never done or older than 6 h.
pub async fn current(app: &App) -> Suggestions {
    let at = app.suggestions.read().await.at;
    if at == 0 || now_ms() - at > STALE_MS {
        refresh(app).await;
    }
    // With the user's current verdict, so the TV sheet shows what was already chosen ("Intéressé" titles stay)
    let mut out = app.suggestions.read().await.clone();
    let ratings = app.ratings.read().await;
    for it in out.items.iter_mut() {
        it.rating = Some(ratings.get(&it.key).map(|r| r.verdict.clone()));
    }
    out
}

/// Background loop: recompute 1.5 s after a rating/settings change, and every 6 h.
pub async fn run(app: Arc<App>) {
    refresh(&app).await;
    warm_trailers(&app);
    loop {
        tokio::select! {
            _ = app.recompute.notified() => tokio::time::sleep(Duration::from_millis(1500)).await,
            _ = tokio::time::sleep(Duration::from_millis(STALE_MS as u64)) => {}
        }
        refresh(&app).await;
        warm_trailers(&app);
    }
}

/// Look up the shelf's trailers in the background so the TV sheet's button shows up at once.
fn warm_trailers(app: &Arc<App>) {
    let app = app.clone();
    tokio::spawn(async move {
        let keys: Vec<String> = app.suggestions.read().await.items.iter().map(|m| m.key.clone()).collect();
        for k in keys {
            let (kind, id) = split_key(&k);
            let _ = crate::trailer::find(&app, kind, id).await;
        }
    });
}

/// Titles to rate on the phone: the most-voted films and series (alternating), then popular ones, minus rated.
/// Skips over pages the user has fully rated; returns the page to continue from.
pub async fn rate_queue(app: &App, page: u32) -> anyhow::Result<(Vec<Item>, u32)> {
    load_genres(app).await?;
    let genres = app.genres.read().await.clone();
    let ratings = app.ratings.read().await.clone();
    let mut out = Vec::new();
    let mut p = page;
    while out.len() < 20 && p < page + 40 && p <= 525 {
        let (sort, pg) = if p <= 25 { ("vote_count.desc", p) } else { ("popularity.desc", p - 25) };
        let mut lists: Vec<VecDeque<Item>> = Vec::new();
        for kind in TYPES {
            let r = tmdb(app, &format!("/discover/{kind}?language=fr-FR&sort_by={sort}&page={pg}"), 7 * DAY).await?;
            lists.push(results(&r).iter()
                .filter(|m| m["poster_path"].is_string() && !ratings.contains_key(&key_of(kind, m["id"].as_i64().unwrap_or(0))))
                .map(|m| shape(m, kind, None, &genres))
                .collect());
        }
        while !lists[0].is_empty() || !lists[1].is_empty() {
            out.extend(lists[0].pop_front());
            out.extend(lists[1].pop_front());
        }
        p += 1;
    }
    Ok((out, p))
}

/// Free-text search: matching titles, plus genres and people the user can tap to browse.
pub async fn search(app: &App, q: &str) -> anyhow::Result<Value> {
    load_genres(app).await?;
    let genres = app.genres.read().await.clone();
    let ratings = app.ratings.read().await.clone();
    let r = tmdb(app, &format!("/search/multi?language=fr-FR&query={}", urlencode(q)), 6 * H).await?;
    let mut items = Vec::new();
    let mut people = Vec::new();
    for m in results(&r) {
        match m["media_type"].as_str() {
            Some(kind @ ("movie" | "tv")) if m["poster_path"].is_string() && items.len() < 15 => {
                let mut it = shape(&m, if kind == "tv" { "tv" } else { "movie" }, None, &genres);
                it.rating = Some(ratings.get(&it.key).map(|r| r.verdict.clone()));
                items.push(it);
            }
            Some("person") if m["profile_path"].is_string() && people.len() < 4
                && matches!(m["known_for_department"].as_str(), Some("Acting" | "Directing")) => {
                people.push(json!({
                    "id": m["id"], "name": m["name"],
                    "photo": format!("{IMG}w185{}", m["profile_path"].as_str().unwrap_or("")),
                    "job": if m["known_for_department"] == "Directing" { "Réalisation" } else { "Acteur·rice" },
                }));
            }
            _ => {}
        }
    }
    let nq = fold(q);
    let cats: Vec<Value> = if nq.chars().count() < 3 { Vec::new() } else {
        genre_table(app).await?.into_iter()
            .filter(|g| fold(&g.name).contains(&nq) || g.aliases.iter().any(|a| a.starts_with(&nq)))
            .map(|g| json!({ "id": g.id, "name": g.name }))
            .collect()
    };
    Ok(json!({ "items": items, "genres": cats, "people": people }))
}

struct GenreChoice {
    id: i64,
    name: String,
    aliases: &'static [&'static str],
    /// Extra /discover filters per type; None = that type has no equivalent
    movie: Option<String>,
    tv: Option<String>,
}

const GENRE_ALIASES: [(i64, &[&str]); 6] = [
    (27, &["horror", "epouvante", "peur", "flippant"]),
    (878, &["sf", "scifi", "sci-fi"]),
    (16, &["anime", "dessin anime", "cartoon"]),
    (35, &["comedy", "drole", "rire"]),
    (10749, &["romance", "romantique", "amour"]),
    (53, &["thriller", "suspense"]),
];

/// Film genres (with their series equivalent) followed by series-only ones.
async fn genre_table(app: &App) -> anyhow::Result<Vec<GenreChoice>> {
    let (m, t) = tokio::try_join!(
        tmdb(app, "/genre/movie/list?language=fr-FR", 7 * DAY),
        tmdb(app, "/genre/tv/list?language=fr-FR", 7 * DAY))?;
    let list = |v: &Value| -> Vec<(i64, String)> {
        v["genres"].as_array().into_iter().flatten().filter_map(|g| Some((g["id"].as_i64()?, text(g, "name")))).collect()
    };
    let (movie, tv) = (list(&m), list(&t));
    let mut out: Vec<GenreChoice> = movie.iter().map(|(id, name)| {
        let tv_ids: Vec<String> = tv.iter().filter(|(g, _)| taste_genres(&[*g]).contains(id)).map(|(g, _)| g.to_string()).collect();
        GenreChoice {
            id: *id,
            name: name.clone(),
            aliases: GENRE_ALIASES.iter().find(|(g, _)| g == id).map(|(_, a)| *a).unwrap_or(&[]),
            movie: Some(format!("&with_genres={id}")),
            tv: if !tv_ids.is_empty() { Some(format!("&with_genres={}", tv_ids.join("|"))) }
                else if *id == 27 { Some("&with_keywords=315058|3335".into()) }
                else { None },
        }
    }).collect();
    for (id, name) in &tv {
        if taste_genres(&[*id]) == [*id] && !movie.iter().any(|(g, _)| g == id) {
            out.push(GenreChoice { id: *id, name: name.clone(), aliases: &[], movie: None, tv: Some(format!("&with_genres={id}")) });
        }
    }
    Ok(out)
}

/// Lowercase without French accents, for forgiving matches ("epouvante", "comedie").
pub fn fold(s: &str) -> String {
    s.trim().to_lowercase().chars().map(|c| match c {
        'à' | 'â' | 'ä' => 'a', 'é' | 'è' | 'ê' | 'ë' => 'e', 'î' | 'ï' => 'i',
        'ô' | 'ö' => 'o', 'ù' | 'û' | 'ü' => 'u', 'ç' => 'c', c => c,
    }).collect()
}

/// Most popular well-rated films and series of a genre, interleaved.
pub async fn by_genre(app: &App, id: i64, page: u32) -> anyhow::Result<Value> {
    load_genres(app).await?;
    let genres = app.genres.read().await.clone();
    let ratings = app.ratings.read().await.clone();
    let Some(g) = genre_table(app).await?.into_iter().find(|g| g.id == id) else { anyhow::bail!("unknown genre {id}") };
    let mut lists: Vec<VecDeque<Item>> = Vec::new();
    for (kind, filter, min) in [("movie", &g.movie, 300), ("tv", &g.tv, 100)] {
        let Some(filter) = filter else { lists.push(VecDeque::new()); continue };
        let r = tmdb(app, &format!("/discover/{kind}?language=fr-FR&sort_by=popularity.desc&vote_count.gte={min}{filter}&page={page}"), 6 * H).await?;
        lists.push(results(&r).iter().filter(|m| m["poster_path"].is_string()).map(|m| {
            let mut it = shape(m, kind, None, &genres);
            it.rating = Some(ratings.get(&it.key).map(|r| r.verdict.clone()));
            it
        }).collect());
    }
    let mut items = Vec::new();
    while !lists[0].is_empty() || !lists[1].is_empty() {
        items.extend(lists[0].pop_front());
        items.extend(lists[1].pop_front());
    }
    Ok(json!({ "title": g.name, "items": items, "more": page < 10 && items.len() >= 10 }))
}

/// A person's films and series (as actor or director), best known first, minus talk shows and cameos as themselves.
pub async fn by_person(app: &App, id: i64) -> anyhow::Result<Value> {
    load_genres(app).await?;
    let genres = app.genres.read().await.clone();
    let ratings = app.ratings.read().await.clone();
    let (pq, cq) = (format!("/person/{id}?language=fr-FR"), format!("/person/{id}/combined_credits?language=fr-FR"));
    let (p, c) = tokio::try_join!(tmdb(app, &pq, 7 * DAY), tmdb(app, &cq, DAY))?;
    let crew = c["crew"].as_array().into_iter().flatten().filter(|m| m["job"] == "Director");
    let mut credits: Vec<&Value> = c["cast"].as_array().into_iter().flatten()
        .filter(|m| {
            let ch = fold(m["character"].as_str().unwrap_or(""));
            !["self", "himself", "herself", "lui-meme", "elle-meme"].iter().any(|s| ch.contains(s))
        })
        .chain(crew)
        .filter(|m| m["poster_path"].is_string() && !genre_ids(m).iter().any(|g| [10763, 10764, 10767].contains(g)))
        .collect();
    credits.sort_by(|a, b| b["vote_count"].as_f64().unwrap_or(0.0).total_cmp(&a["vote_count"].as_f64().unwrap_or(0.0)));
    let mut seen = std::collections::HashSet::new();
    let items: Vec<Item> = credits.into_iter()
        .filter_map(|m| {
            let kind = match m["media_type"].as_str()? { "tv" => "tv", "movie" => "movie", _ => return None };
            let mut it = shape(m, kind, None, &genres);
            if !seen.insert(it.key.clone()) { return None }
            it.rating = Some(ratings.get(&it.key).map(|r| r.verdict.clone()));
            Some(it)
        })
        .take(60)
        .collect();
    Ok(json!({ "title": p["name"], "items": items, "more": false }))
}

pub fn urlencode(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// Full details for the phone sheet: overview, length, where to watch.
pub async fn details(app: &App, kind: &'static str, id: i64) -> anyhow::Result<Item> {
    load_genres(app).await?;
    let genres = app.genres.read().await.clone();
    let (dq, wq) = (format!("/{kind}/{id}?language=fr-FR"), format!("/{kind}/{id}/watch/providers"));
    let (d, w) = tokio::try_join!(tmdb(app, &dq, 2 * DAY), tmdb(app, &wq, 2 * DAY))?;
    let mut it = shape(&d, kind, None, &genres);
    it.genres = d["genres"].as_array().into_iter().flatten().map(|g| text(g, "name")).take(3).collect();
    it.length = Some(if kind == "tv" {
        match d["number_of_seasons"].as_i64() {
            Some(n) if n > 0 => format!("{n} saison{}", if n > 1 { "s" } else { "" }),
            _ => String::new(),
        }
    } else {
        match d["runtime"].as_i64() {
            Some(m) if m > 0 => format!("{} h {:02}", m / 60, m % 60),
            _ => String::new(),
        }
    });
    let seen = app.state.lock().await.seen_apps.clone();
    let settings = app.settings.read().await;
    // Best service, so the phone can start the title on the TV
    it.provider = provider_for(app, &settings.region, &providers::enabled(&settings), &seen, kind, id).await;
    let mine: Vec<i64> = providers::enabled(&settings).iter().flat_map(|p| p.ids.iter().copied()).collect();
    it.watch = Some(w["results"][settings.region.as_str()]["flatrate"].as_array().into_iter().flatten()
        .filter(|x| !text(x, "provider_name").to_lowercase().contains("with ads"))
        .map(|x| Watch { name: text(x, "provider_name"), mine: x["provider_id"].as_i64().is_some_and(|i| mine.contains(&i)) })
        .collect());
    it.rating = Some(app.ratings.read().await.get(&it.key).map(|r| r.verdict.clone()));
    Ok(it)
}
