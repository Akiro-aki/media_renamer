//! AniList GraphQL: anime and manga identity. No API key needed.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Candidate, Http, Provider, Source, strip_html};
use crate::MediaKind;
use crate::config::TitlePreference;

const BASE: &str = "https://graphql.anilist.co";

const FIELDS: &str = "
fragment f on Media {
  id type format seasonYear startDate { year }
  title { romaji english native } synonyms
  coverImage { extraLarge large } bannerImage
  description(asHtml: false) genres siteUrl
}";

pub struct Anilist {
    http: Arc<Http>,
    title_pref: TitlePreference,
    base: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaJson {
    id: u64,
    format: Option<String>,
    season_year: Option<u16>,
    start_date: Option<StartDate>,
    title: Titles,
    #[serde(default)]
    synonyms: Vec<String>,
    cover_image: Option<Cover>,
    banner_image: Option<String>,
    description: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
    site_url: Option<String>,
}

#[derive(Deserialize)]
struct StartDate {
    year: Option<u16>,
}

#[derive(Deserialize)]
struct Titles {
    romaji: Option<String>,
    english: Option<String>,
    native: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cover {
    extra_large: Option<String>,
    large: Option<String>,
}

impl Anilist {
    pub fn new(http: Arc<Http>, title_pref: TitlePreference) -> Anilist {
        Anilist {
            http,
            title_pref,
            base: BASE.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Anilist {
        self.base = base.into();
        self
    }

    async fn query(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": format!("{query}\n{FIELDS}"), "variables": variables });
        let v = self.http.post_json(&self.base, &body).await?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
            && !errors.is_empty()
        {
            let msg = errors[0]["message"].as_str().unwrap_or("unknown error");
            bail!("AniList: {msg}");
        }
        Ok(v)
    }

    fn media_type(kind: MediaKind) -> &'static str {
        if kind.is_video() { "ANIME" } else { "MANGA" }
    }

    fn candidate(&self, m: MediaJson, kind: MediaKind) -> Candidate {
        let t = &m.title;
        let romaji = t.romaji.clone();
        let title = match self.title_pref {
            TitlePreference::Romaji => romaji.clone(),
            TitlePreference::English => t.english.clone().or(romaji.clone()),
            TitlePreference::Native => t.native.clone().or(romaji.clone()),
        }
        .unwrap_or_default();

        let mut alt: Vec<String> = [t.romaji.clone(), t.english.clone(), t.native.clone()]
            .into_iter()
            .flatten()
            .chain(m.synonyms)
            .filter(|s| *s != title && !s.is_empty())
            .collect();
        alt.dedup();

        Candidate {
            source: Some(Source::Anilist),
            id: m.id.to_string(),
            kind: Some(kind),
            title,
            alt_titles: alt,
            year: m.season_year.or(m.start_date.and_then(|d| d.year)),
            overview: m.description.map(|d| strip_html(&d)).filter(|d| !d.is_empty()),
            poster: m.cover_image.and_then(|c| c.extra_large.or(c.large)),
            backdrop: m.banner_image,
            genres: m.genres,
            english_title: t.english.clone(),
            url: m.site_url,
            ..Default::default()
        }
    }
}

#[async_trait]
impl Provider for Anilist {
    fn source(&self) -> Source {
        Source::Anilist
    }

    async fn search(&self, query: &str, _year: Option<u16>, kind: MediaKind) -> Result<Vec<Candidate>> {
        let q = "query ($search: String, $type: MediaType) {
            Page(perPage: 10) { media(search: $search, type: $type, sort: SEARCH_MATCH) { ...f } }
        }";
        let v = self
            .query(q, json!({ "search": query, "type": Self::media_type(kind) }))
            .await?;
        let list: Vec<MediaJson> =
            serde_json::from_value(v["data"]["Page"]["media"].clone()).context("AniList search")?;
        // AniList's MANGA type includes light novels, which usually share the
        // manga's title and would win the match. `--id anilist:N` still
        // reaches a novel through `lookup`.
        Ok(list
            .into_iter()
            .filter(|m| m.format.as_deref() != Some("NOVEL"))
            .map(|m| self.candidate(m, kind))
            .collect())
    }

    async fn lookup(&self, id: &str, kind: MediaKind) -> Result<Candidate> {
        let id: i64 = id.parse().context("AniList ids are numbers")?;
        let q = "query ($id: Int, $type: MediaType) { Media(id: $id, type: $type) { ...f } }";
        let v = self
            .query(q, json!({ "id": id, "type": Self::media_type(kind) }))
            .await?;
        let m: MediaJson =
            serde_json::from_value(v["data"]["Media"].clone()).context("AniList lookup")?;
        Ok(self.candidate(m, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn search_prefers_configured_title() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Page": {"media": [{
                "id": 154587, "type": "ANIME", "seasonYear": 2023,
                "title": {"romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey's End", "native": "葬送のフリーレン"},
                "synonyms": ["Frieren"], "coverImage": {"extraLarge": "https://x/c.jpg"},
                "description": "A <i>fairy</i> tale.<br>", "genres": ["Adventure"]
            }]}}})))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let a = Anilist::new(http.clone(), TitlePreference::Romaji).with_base_url(server.uri());
        let r = a.search("Frieren", None, MediaKind::Anime).await.unwrap();
        assert_eq!(r[0].title, "Sousou no Frieren");
        assert!(r[0].alt_titles.contains(&"Frieren".to_string()));
        assert_eq!(r[0].overview.as_deref(), Some("A fairy tale."));
        assert_eq!(r[0].year, Some(2023));

        let a = Anilist::new(http, TitlePreference::English).with_base_url(server.uri());
        let r = a.search("Frieren", None, MediaKind::Anime).await.unwrap();
        assert_eq!(r[0].title, "Frieren: Beyond Journey's End");
    }

    #[tokio::test]
    async fn manga_search_skips_light_novels() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Page": {"media": [
                {"id": 1, "type": "MANGA", "format": "NOVEL", "title": {"romaji": "Spy Classroom"}},
                {"id": 2, "type": "MANGA", "format": "MANGA", "title": {"romaji": "Spy Classroom"}},
                {"id": 3, "type": "MANGA", "format": "ONE_SHOT", "title": {"romaji": "Spy Classroom: Extra"}}
            ]}}})))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let a = Anilist::new(http, TitlePreference::Romaji).with_base_url(server.uri());
        let r = a.search("Spy Classroom", None, MediaKind::Manga).await.unwrap();
        let ids: Vec<&str> = r.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["2", "3"]);
    }
}
