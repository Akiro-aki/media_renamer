//! MangaDex: manga fallback when AniList has no match. No API key needed.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{Candidate, Http, Provider, Source};
use crate::MediaKind;

const BASE: &str = "https://api.mangadex.org";
const COVERS: &str = "https://uploads.mangadex.org/covers";

pub struct Mangadex {
    http: Arc<Http>,
    base: String,
}

#[derive(Deserialize)]
struct MangaJson {
    id: String,
    attributes: Attributes,
    #[serde(default)]
    relationships: Vec<Relationship>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attributes {
    #[serde(default)]
    title: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    alt_titles: Vec<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    description: Value,
    year: Option<u16>,
    /// Ids on other sites: `al` (AniList), `mal`, `mu`, ...
    #[serde(default)]
    links: Option<std::collections::BTreeMap<String, Value>>,
}

#[derive(Deserialize)]
struct Relationship {
    #[serde(rename = "type")]
    kind: String,
    attributes: Option<Value>,
}

impl Mangadex {
    pub fn new(http: Arc<Http>) -> Mangadex {
        Mangadex {
            http,
            base: BASE.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Mangadex {
        self.base = base.into();
        self
    }

    fn candidate(m: MangaJson) -> Candidate {
        let a = &m.attributes;
        let pick = |map: &std::collections::BTreeMap<String, String>| {
            ["en", "ja-ro", "ko-ro", "zh-ro"]
                .iter()
                .find_map(|l| map.get(*l).cloned())
                .or_else(|| map.values().next().cloned())
        };
        let title = pick(&a.title).unwrap_or_default();
        let mut alt: Vec<String> = a
            .alt_titles
            .iter()
            .flat_map(|m| m.values().cloned())
            .chain(a.title.values().cloned())
            .filter(|t| *t != title)
            .collect();
        alt.dedup();
        let english = a
            .alt_titles
            .iter()
            .find_map(|m| m.get("en").cloned())
            .or_else(|| a.title.get("en").cloned());

        let cover = m.relationships.iter().find_map(|r| {
            (r.kind == "cover_art")
                .then(|| r.attributes.as_ref()?.get("fileName")?.as_str().map(String::from))
                .flatten()
        });

        let ext_ids = a
            .links
            .iter()
            .flatten()
            .filter_map(|(site, id)| {
                let source = match site.as_str() {
                    "al" => Source::Anilist,
                    "mal" => Source::Mal,
                    _ => return None,
                };
                // Usually a string, but be lenient about numbers.
                let id = match id {
                    Value::String(s) => s.trim().to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => return None,
                };
                (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then_some((source, id))
            })
            .collect();

        Candidate {
            source: Some(Source::Mangadex),
            ext_ids,
            kind: Some(MediaKind::Manga),
            title,
            alt_titles: alt,
            year: a.year,
            overview: a.description.get("en").and_then(|d| d.as_str()).map(String::from),
            poster: cover.map(|f| format!("{COVERS}/{}/{f}", m.id)),
            english_title: english,
            url: Some(format!("https://mangadex.org/title/{}", m.id)),
            id: m.id,
            ..Default::default()
        }
    }
}

#[async_trait]
impl Provider for Mangadex {
    fn source(&self) -> Source {
        Source::Mangadex
    }

    async fn search(&self, query: &str, _year: Option<u16>, _kind: MediaKind) -> Result<Vec<Candidate>> {
        let q = [
            ("title", query.to_string()),
            ("limit", "10".to_string()),
            ("includes[]", "cover_art".to_string()),
            ("order[relevance]", "desc".to_string()),
        ];
        let v = self
            .http
            .get_json(&format!("{}/manga", self.base), &q, &[], &[])
            .await?;
        let list: Vec<MangaJson> =
            serde_json::from_value(v["data"].clone()).context("MangaDex search")?;
        Ok(list.into_iter().map(Self::candidate).collect())
    }

    async fn lookup(&self, id: &str, _kind: MediaKind) -> Result<Candidate> {
        let q = [("includes[]", "cover_art".to_string())];
        let v = self
            .http
            .get_json(&format!("{}/manga/{id}", self.base), &q, &[], &[])
            .await?;
        let m: MangaJson = serde_json::from_value(v["data"].clone()).context("MangaDex lookup")?;
        Ok(Self::candidate(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn search_parses_titles_and_cover() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manga"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{
                "id": "abc",
                "attributes": {
                    "title": {"ja-ro": "Chainsaw Man"},
                    "altTitles": [{"ja": "チェンソーマン"}, {"en": "Chainsaw Man"}],
                    "description": {"en": "Denji..."},
                    "year": 2018,
                    "links": {"al": "105778", "mal": "116778", "mu": "pa6x7ji", "raw": "https://x"}
                },
                "relationships": [{"type": "author"}, {"type": "cover_art", "attributes": {"fileName": "c.jpg"}}]
            }]})))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let m = Mangadex::new(http).with_base_url(server.uri());
        let r = m.search("chainsaw man", None, MediaKind::Manga).await.unwrap();
        assert_eq!(r[0].title, "Chainsaw Man");
        assert_eq!(r[0].year, Some(2018));
        assert_eq!(r[0].poster.as_deref(), Some("https://uploads.mangadex.org/covers/abc/c.jpg"));
        assert!(r[0].alt_titles.contains(&"チェンソーマン".to_string()));
        assert_eq!(r[0].id_for(Source::Anilist), Some("105778"));
        assert_eq!(r[0].id_for(Source::Mal), Some("116778"));
        assert_eq!(r[0].ext_ids.len(), 2);
    }
}
