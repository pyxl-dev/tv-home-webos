// Minimal TMDB client with an in-memory response cache.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const H: Duration = Duration::from_secs(3600);
pub const DAY: Duration = Duration::from_secs(86400);

pub struct Tmdb {
    http: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Arc<Value>)>>,
}

impl Tmdb {
    pub fn new() -> Self {
        Tmdb {
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("http client"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// GET `q` (path + query); `key` is a v3 api key or a v4 read token ("eyJ…").
    pub async fn get(&self, key: &str, q: &str, ttl: Duration) -> anyhow::Result<Arc<Value>> {
        if let Some((at, v)) = self.cache.lock().await.get(q) {
            if at.elapsed() < ttl {
                return Ok(v.clone());
            }
        }
        let bearer = key.starts_with("eyJ");
        let mut url = format!("https://api.themoviedb.org/3{q}");
        if !bearer {
            url.push(if q.contains('?') { '&' } else { '?' });
            url.push_str("api_key=");
            url.push_str(key);
        }
        let mut req = self.http.get(&url).header("accept", "application/json");
        if bearer {
            req = req.bearer_auth(key);
        }
        let res = req.send().await?;
        if !res.status().is_success() {
            anyhow::bail!("TMDB {}", res.status());
        }
        let v = Arc::new(res.json::<Value>().await?);
        let mut cache = self.cache.lock().await;
        if cache.len() > 5000 {
            cache.retain(|_, (at, _)| at.elapsed() < 2 * DAY);
        }
        cache.insert(q.to_string(), (Instant::now(), v.clone()));
        Ok(v)
    }
}
