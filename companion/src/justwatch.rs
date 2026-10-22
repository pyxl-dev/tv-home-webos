// Deep links into streaming apps: JustWatch knows each title's page on Netflix / Prime Video,
// and the TV's launcher accepts it as `contentId` (same as LG's own search results).
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const API: &str = "https://apis.justwatch.com/graphql";
const TTL: Duration = Duration::from_secs(2 * 86400);

const QUERY: &str = r#"query($q:String!,$t:[ObjectType!]){popularTitles(country:FR,first:8,filter:{searchQuery:$q,objectTypes:$t}){edges{node{content(country:FR,language:"fr"){externalIds{tmdbId}} offers(country:FR,platform:WEB){monetizationType package{technicalName} standardWebURL}}}}}"#;

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(8)).build().expect("http client"))
}

fn cache() -> &'static Mutex<HashMap<String, (Instant, Vec<(String, String)>)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Vec<(String, String)>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Subscription offers for a title: (JustWatch package, web URL). Matched on the TMDB id.
async fn offers(kind: &str, tmdb_id: i64, title: &str) -> anyhow::Result<Vec<(String, String)>> {
    let key = format!("{kind}:{tmdb_id}");
    if let Some((at, v)) = cache().lock().await.get(&key) {
        if at.elapsed() < TTL {
            return Ok(v.clone());
        }
    }
    let object = if kind == "tv" { "SHOW" } else { "MOVIE" };
    let res: Value = client().post(API)
        .json(&json!({ "query": QUERY, "variables": { "q": title, "t": [object] } }))
        .send().await?.error_for_status()?.json().await?;
    let id = tmdb_id.to_string();
    let found = res["data"]["popularTitles"]["edges"].as_array().into_iter().flatten()
        .map(|e| &e["node"])
        .find(|n| n["content"]["externalIds"]["tmdbId"].as_str() == Some(id.as_str()));
    let list: Vec<(String, String)> = found.and_then(|n| n["offers"].as_array()).into_iter().flatten()
        .filter(|o| o["monetizationType"] == "FLATRATE")
        .filter_map(|o| Some((o["package"]["technicalName"].as_str()?.to_string(), o["standardWebURL"].as_str()?.to_string())))
        .collect();
    cache().lock().await.insert(key, (Instant::now(), list.clone()));
    Ok(list)
}

/// `contentId` that opens this title directly in the provider's TV app, when we know the format.
pub async fn content_id(provider: &str, kind: &str, tmdb_id: i64, title: &str) -> Option<String> {
    let list = offers(kind, tmdb_id, title).await.map_err(|e| log!("justwatch: {e}")).ok()?;
    let url = |pkgs: &[&str]| list.iter().find(|(p, _)| pkgs.contains(&p.as_str())).map(|(_, u)| u.clone());
    match provider {
        // Netflix TV app: "m=<title url>&source_type=4" (opens the title page)
        "netflix" => {
            let u = url(&["netflix", "netflixbasicwithads"])?;
            let nid: String = u.split("/title/").nth(1)?.chars().take_while(|c| c.is_ascii_digit()).collect();
            (!nid.is_empty()).then(|| format!("m=https://www.netflix.com/title/{nid}&source_type=4"))
        }
        // Prime Video: the detail URL (https://app.primevideo.com/detail?gti=…) is accepted, but the app
        // restarts on its profile picker and then lands on its home, losing the title. Opening it plainly
        // is better (it resumes without asking the profile again), so no deep link until a working format
        // is found. The offers lookup above already returns the Prime URL for when it is.
        "prime" => None,
        _ => None,
    }
}
