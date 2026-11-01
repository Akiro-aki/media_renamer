//! Shared HTTP client: per-host rate limiting, retries, response caching.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{RequestBuilder, StatusCode};
use serde_json::Value;

use crate::cache::Cache;

const USER_AGENT: &str = concat!(
    "mrn/",
    env!("CARGO_PKG_VERSION"),
    " (media renamer; +https://github.com/)"
);
const MAX_ATTEMPTS: u32 = 4;

pub struct HttpOptions {
    pub cache: Option<Arc<Cache>>,
    pub ttl: Duration,
    /// Ignore cached responses (but still write fresh ones).
    pub refresh: bool,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            cache: None,
            ttl: Duration::from_secs(72 * 3600),
            refresh: false,
        }
    }
}

pub struct Http {
    client: reqwest::Client,
    opts: HttpOptions,
    limiters: Mutex<HashMap<String, Arc<Limiter>>>,
}

impl Http {
    pub fn new(opts: HttpOptions) -> Result<Http> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Http {
            client,
            opts,
            limiters: Mutex::new(HashMap::new()),
        })
    }

    /// GET a JSON document. `secret_query` names query parameters that must
    /// not end up in cache keys or error messages (API keys).
    pub async fn get_json(
        &self,
        url: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
        secret_query: &[&str],
    ) -> Result<Value> {
        let public: Vec<String> = query
            .iter()
            .filter(|(k, _)| !secret_query.contains(k))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let key = format!("GET {url}?{}", public.join("&"));
        self.cached(&key, url, || {
            let mut rb = self.client.get(url).query(query);
            for (k, v) in headers {
                rb = rb.header(*k, v);
            }
            rb
        })
        .await
    }

    /// POST a JSON body and parse a JSON response (GraphQL).
    pub async fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        let key = format!("POST {url}#{body}");
        self.cached(&key, url, || {
            self.client
                .post(url)
                .header("Accept", "application/json")
                .json(body)
        })
        .await
    }

    /// Download a file (artwork) to `dest`. Not cached.
    pub async fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let resp = self.send(url, || self.client.get(url)).await?;
        let bytes = resp.bytes().await.map_err(|e| e.without_url())?;
        let tmp = dest.with_extension("mrn-part");
        tokio::fs::write(&tmp, &bytes)
            .await
            .with_context(|| format!("writing {}", tmp.display()))?;
        tokio::fs::rename(&tmp, dest).await?;
        Ok(())
    }

    async fn cached(&self, key: &str, url: &str, build: impl Fn() -> RequestBuilder) -> Result<Value> {
        if !self.opts.refresh
            && let Some(cache) = &self.opts.cache
            && let Some(hit) = cache.get(key)
            && let Ok(v) = serde_json::from_str(&hit)
        {
            tracing::debug!("cache hit: {key}");
            return Ok(v);
        }
        let resp = self.send(url, build).await?;
        let text = resp.text().await.map_err(|e| e.without_url())?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON from {}", host_of(url)))?;
        if let Some(cache) = &self.opts.cache {
            cache.put(key, &text, self.opts.ttl);
        }
        Ok(value)
    }

    async fn send(&self, url: &str, build: impl Fn() -> RequestBuilder) -> Result<reqwest::Response> {
        let host = host_of(url);
        let limiter = self.limiter(&host);
        let mut last_err = anyhow!("no attempts made");
        for attempt in 0..MAX_ATTEMPTS {
            limiter.acquire().await;
            let backoff = Duration::from_secs(1 << attempt);
            match build().send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                        let wait = retry_after(&resp).unwrap_or(backoff).min(Duration::from_secs(60));
                        tracing::warn!("{host} returned {status}, retrying in {wait:?}");
                        last_err = anyhow!("{host} returned {status}");
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    if !status.is_success() {
                        let body = resp.text().await.unwrap_or_default();
                        let snippet: String = body.chars().take(200).collect();
                        bail!("{host} returned {status}: {snippet}");
                    }
                    return Ok(resp);
                }
                Err(e) if e.is_timeout() || e.is_connect() => {
                    let e = e.without_url();
                    tracing::warn!("{host}: {e}, retrying in {backoff:?}");
                    last_err = anyhow!("{host}: {e}");
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => return Err(anyhow!("{host}: {}", e.without_url())),
            }
        }
        Err(last_err)
    }

    fn limiter(&self, host: &str) -> Arc<Limiter> {
        let mut map = self.limiters.lock().unwrap();
        map.entry(host.to_string())
            .or_insert_with(|| {
                let (capacity, per_second) = limits_for(host);
                Arc::new(Limiter::new(capacity, per_second))
            })
            .clone()
    }
}

/// Burst size and sustained requests/second per API host.
fn limits_for(host: &str) -> (f64, f64) {
    match host {
        "graphql.anilist.co" => (5.0, 0.5),          // 30/min
        "api.mangadex.org" => (5.0, 4.0),            // 5/s
        "comicvine.gamespot.com" => (10.0, 200.0 / 3600.0), // 200/hour
        "api.themoviedb.org" => (20.0, 20.0),
        _ => (10.0, 10.0),
    }
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host,
            })
        })
        .unwrap_or_else(|| "unknown host".into())
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Token bucket.
struct Limiter {
    capacity: f64,
    per_second: f64,
    state: tokio::sync::Mutex<(f64, Instant)>,
}

impl Limiter {
    fn new(capacity: f64, per_second: f64) -> Limiter {
        Limiter {
            capacity,
            per_second,
            state: tokio::sync::Mutex::new((capacity, Instant::now())),
        }
    }

    async fn acquire(&self) {
        loop {
            let wait = {
                let mut s = self.state.lock().await;
                let now = Instant::now();
                let refill = now.duration_since(s.1).as_secs_f64() * self.per_second;
                s.0 = (s.0 + refill).min(self.capacity);
                s.1 = now;
                if s.0 >= 1.0 {
                    s.0 -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - s.0) / self.per_second)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn retries_and_caches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let d = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(&d.path().join("c.db")).unwrap());
        let http = Http::new(HttpOptions {
            cache: Some(cache),
            ..Default::default()
        })
        .unwrap();
        let url = format!("{}/x", server.uri());
        let q = [("api_key", "secret".to_string()), ("q", "a".to_string())];
        let v = http.get_json(&url, &q, &[], &["api_key"]).await.unwrap();
        assert_eq!(v["ok"], true);
        // Second call is served from the cache (the mock expects one 200).
        let v = http.get_json(&url, &q, &[], &["api_key"]).await.unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn client_errors_do_not_leak_keys() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&server)
            .await;
        let http = Http::new(HttpOptions::default()).unwrap();
        let err = http
            .get_json(&server.uri(), &[("api_key", "SECRET".into())], &[], &["api_key"])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("401"), "{err}");
        assert!(!err.contains("SECRET"), "{err}");
    }
}
