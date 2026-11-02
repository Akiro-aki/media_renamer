//! TheTVDB v4 (`api4.thetvdb.com/v4`): anime episode titles in TheTVDB's
//! seasons, the numbering Sonarr, Plex and Jellyfin use. Needs the user's own
//! API key (and, for some keys, a subscriber PIN). Never searched for a match:
//! an anime matched on AniList is found here by title, like on TMDB.
//!
//! Built from TheTVDB's OpenAPI spec (`thetvdb/v4-api`, `docs/swagger.yml`,
//! 4.7.10). Not yet checked against the live API.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Candidate, Episode, Http, SeriesInfo, Source};

const BASE: &str = "https://api4.thetvdb.com/v4";
/// Episode pages to read at most (500 episodes each).
const MAX_PAGES: u32 = 20;

pub struct Tvdb {
    http: Arc<Http>,
    key: String,
    pin: Option<String>,
    /// Three-letter language code for episode names (`eng`).
    language: String,
    base: String,
    /// Bearer token from `/login`, valid for a month; fetched once per run.
    token: tokio::sync::Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct SearchHit {
    tvdb_id: Option<String>,
    name: Option<String>,
    year: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    /// Language → translated name.
    #[serde(default)]
    translations: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeJson {
    season_number: Option<u32>,
    number: Option<u32>,
    absolute_number: Option<u32>,
    name: Option<String>,
    aired: Option<String>,
    overview: Option<String>,
}

/// ISO 639-1 (`en`, from `general.language` such as `en-US`) → the ISO 639-2
/// codes TheTVDB uses. Unknown languages fall back to English.
fn tvdb_language(language: &str) -> &'static str {
    let short = language.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    match short.as_str() {
        "ja" => "jpn",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "zh" => "zho",
        "ko" => "kor",
        "nl" => "nld",
        "pl" => "pol",
        "sv" => "swe",
        "da" => "dan",
        "fi" => "fin",
        "no" | "nb" => "nor",
        "cs" => "ces",
        "hu" => "hun",
        "tr" => "tur",
        "he" => "heb",
        "ar" => "ara",
        "el" => "ell",
        _ => "eng",
    }
}

impl Tvdb {
    pub fn new(http: Arc<Http>, key: String, pin: Option<String>, language: &str) -> Tvdb {
        Tvdb {
            http,
            key,
            pin,
            language: tvdb_language(language).to_string(),
            base: BASE.to_string(),
            token: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Tvdb {
        self.base = base.into();
        self
    }

    /// The bearer token, logging in on first use.
    async fn token(&self) -> Result<String> {
        let mut token = self.token.lock().await;
        if let Some(t) = token.as_ref() {
            return Ok(t.clone());
        }
        let mut body = json!({ "apikey": self.key });
        if let Some(pin) = &self.pin {
            body["pin"] = json!(pin);
        }
        let v = self
            .http
            .post_json_uncached(&format!("{}/login", self.base), &body)
            .await
            .context("TheTVDB login (check [api] tvdb and tvdb_pin)")?;
        let Some(t) = v["data"]["token"].as_str().filter(|t| !t.is_empty()) else {
            bail!("TheTVDB login returned no token");
        };
        *token = Some(t.to_string());
        Ok(t.to_string())
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let token = self.token().await?;
        self.http
            .get_json(
                &format!("{}{path}", self.base),
                query,
                &[("Authorization", format!("Bearer {token}"))],
                &[],
            )
            .await
    }

    /// Series by title. Candidates carry TheTVDB's id, and its aliases and
    /// translated names as alternative titles.
    pub async fn search_series(&self, query: &str) -> Result<Vec<Candidate>> {
        let q = [("query", query.to_string()), ("type", "series".to_string())];
        let v = self.get("/search", &q).await?;
        let hits: Vec<SearchHit> =
            serde_json::from_value(v["data"].clone()).context("TheTVDB search")?;
        Ok(hits
            .into_iter()
            .filter_map(|h| {
                let id = h.tvdb_id.filter(|i| !i.is_empty())?;
                let title = h.name?;
                let mut alt: Vec<String> = Vec::new();
                for t in h.aliases.into_iter().chain(h.translations.into_iter().flat_map(|m| m.into_values())) {
                    if t != title && !t.is_empty() && !alt.contains(&t) {
                        alt.push(t);
                    }
                }
                Some(Candidate {
                    source: Some(Source::Tvdb),
                    id,
                    title,
                    alt_titles: alt,
                    year: h.year.and_then(|y| y.get(..4)?.parse().ok()),
                    ..Default::default()
                })
            })
            .collect())
    }

    /// Every episode in the series' default (aired) order, named in the
    /// configured language. Episodes without a name in it keep `title: None`,
    /// so a later source can fill them in.
    pub async fn series(&self, id: &str) -> Result<SeriesInfo> {
        let mut raw: Vec<EpisodeJson> = Vec::new();
        for page in 0..MAX_PAGES {
            let v = self
                .get(
                    &format!("/series/{id}/episodes/default/{}", self.language),
                    &[("page", page.to_string())],
                )
                .await?;
            // The spec nests episodes under `series`; accept them beside it too.
            let list = if v["data"]["episodes"].is_array() {
                &v["data"]["episodes"]
            } else {
                &v["data"]["series"]["episodes"]
            };
            let got: Vec<EpisodeJson> =
                serde_json::from_value(list.clone()).context("TheTVDB episodes")?;
            let empty = got.is_empty();
            raw.extend(got);
            if empty || v["links"]["next"].is_null() {
                break;
            }
        }
        Ok(episodes_from(raw))
    }
}

/// TheTVDB episodes → mrn's. Specials (season 0) have no absolute number.
/// When TheTVDB gives no absolute numbers, regular episodes are counted in
/// season order, as for TMDB.
fn episodes_from(raw: Vec<EpisodeJson>) -> SeriesInfo {
    let mut episodes: Vec<Episode> = raw
        .into_iter()
        .filter_map(|e| {
            let season = e.season_number?;
            Some(Episode {
                season,
                episode: e.number?,
                absolute: e.absolute_number.filter(|&a| a > 0 && season > 0),
                title: e.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
                air_date: e.aired.filter(|d| !d.is_empty()),
                overview: e.overview.filter(|o| !o.trim().is_empty()),
            })
        })
        .collect();
    episodes.sort_by_key(|e| (e.season == 0, e.season, e.episode));
    if !episodes.iter().any(|e| e.absolute.is_some()) {
        let mut n = 0;
        for e in episodes.iter_mut().filter(|e| e.season > 0) {
            n += 1;
            e.absolute = Some(n);
        }
    }
    SeriesInfo {
        episodes,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tvdb(server: &MockServer, pin: Option<&str>) -> Tvdb {
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        Tvdb::new(http, "KEY".into(), pin.map(String::from), "en-US").with_base_url(server.uri())
    }

    async fn mock_login(server: &MockServer, body: Value) {
        Mock::given(method("POST"))
            .and(path("/login"))
            .and(body_json(body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success", "data": {"token": "TOKEN"}
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn logs_in_once_and_pages_episodes() {
        let server = MockServer::start().await;
        mock_login(&server, json!({"apikey": "KEY", "pin": "1234"})).await;
        let ep = |s: u32, n: u32, abs: u32, name: Option<&str>| {
            json!({"seasonNumber": s, "number": n, "absoluteNumber": abs, "name": name,
                   "aired": "2023-09-29"})
        };
        Mock::given(method("GET"))
            .and(path("/series/424536/episodes/default/eng"))
            .and(query_param("page", "0"))
            .and(header("Authorization", "Bearer TOKEN"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"series": {"id": 424536, "episodes": [
                    ep(1, 1, 1, Some("The Journey's End")), ep(1, 2, 2, None), ep(0, 1, 0, Some("Special"))
                ]}},
                "links": {"next": "https://api4.thetvdb.com/v4/series/424536/episodes/default/eng?page=1"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/series/424536/episodes/default/eng"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"series": {"id": 424536}, "episodes": [ep(2, 1, 29, Some("Season 2 opener"))]},
                "links": {"next": null}
            })))
            .mount(&server)
            .await;
        let t = tvdb(&server, Some("1234"));
        let s = t.series("424536").await.unwrap();
        let got: Vec<_> = s
            .episodes
            .iter()
            .map(|e| (e.season, e.episode, e.absolute, e.title.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (1, 1, Some(1), Some("The Journey's End")),
                (1, 2, Some(2), None),
                (2, 1, Some(29), Some("Season 2 opener")),
                (0, 1, None, Some("Special")),
            ]
        );
        assert!(!s.single_entry);
        // The token is reused (the login mock expects one call).
        t.series("424536").await.unwrap();
    }

    #[tokio::test]
    async fn search_reads_aliases_and_translations() {
        let server = MockServer::start().await;
        mock_login(&server, json!({"apikey": "KEY"})).await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("query", "Sousou no Frieren"))
            .and(query_param("type", "series"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": [{
                    "tvdb_id": "424536", "name": "Frieren: Beyond Journey's End", "year": "2023",
                    "aliases": ["Sousou no Frieren"],
                    "translations": {"jpn": "葬送のフリーレン", "eng": "Frieren: Beyond Journey's End"}
                }, {"name": "no id"}]
            })))
            .mount(&server)
            .await;
        let r = tvdb(&server, None).search_series("Sousou no Frieren").await.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "424536");
        assert_eq!(r[0].year, Some(2023));
        assert_eq!(r[0].alt_titles, ["Sousou no Frieren", "葬送のフリーレン"]);
    }

    #[tokio::test]
    async fn bad_key_is_a_clear_error_without_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "status": "failure", "message": "InvalidAPIKey: apikey invalid", "data": null
            })))
            .mount(&server)
            .await;
        let err = format!("{:#}", tvdb(&server, None).series("1").await.unwrap_err());
        assert!(err.contains("TheTVDB login") && err.contains("401"), "{err}");
        assert!(!err.contains("KEY"), "{err}");
    }

    #[test]
    fn counts_absolute_numbers_when_missing() {
        let raw = vec![
            EpisodeJson { season_number: Some(2), number: Some(1), absolute_number: None, name: None, aired: None, overview: None },
            EpisodeJson { season_number: Some(1), number: Some(2), absolute_number: None, name: None, aired: None, overview: None },
            EpisodeJson { season_number: Some(1), number: Some(1), absolute_number: Some(0), name: None, aired: None, overview: None },
            EpisodeJson { season_number: Some(0), number: Some(1), absolute_number: None, name: None, aired: None, overview: None },
        ];
        let s = episodes_from(raw);
        let abs: Vec<_> = s.episodes.iter().map(|e| (e.season, e.episode, e.absolute)).collect();
        assert_eq!(abs, [(1, 1, Some(1)), (1, 2, Some(2)), (2, 1, Some(3)), (0, 1, None)]);
    }

    #[test]
    fn languages_map_to_tvdb_codes() {
        assert_eq!(tvdb_language("en-US"), "eng");
        assert_eq!(tvdb_language("ja"), "jpn");
        assert_eq!(tvdb_language("pt_BR"), "por");
        assert_eq!(tvdb_language("xx"), "eng");
    }
}
