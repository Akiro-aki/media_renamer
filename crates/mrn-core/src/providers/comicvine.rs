//! ComicVine: western comics (volumes and issues). Needs a free API key.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{Candidate, Http, Issue, Provider, Source, strip_html};
use crate::MediaKind;
use crate::format::template::format_number;

const BASE: &str = "https://comicvine.gamespot.com/api";
const VOLUME_FIELDS: &str = "id,name,start_year,publisher,image,deck,description,site_detail_url,count_of_issues";

pub struct ComicVine {
    http: Arc<Http>,
    key: String,
    base: String,
}

#[derive(Deserialize)]
struct VolumeJson {
    id: u64,
    name: Option<String>,
    start_year: Option<Value>,
    publisher: Option<Named>,
    image: Option<Image>,
    deck: Option<String>,
    description: Option<String>,
    site_detail_url: Option<String>,
}

#[derive(Deserialize)]
struct Named {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Image {
    original_url: Option<String>,
}

#[derive(Deserialize)]
struct IssueJson {
    name: Option<String>,
    issue_number: Option<String>,
    cover_date: Option<String>,
    image: Option<Image>,
    description: Option<String>,
}

impl ComicVine {
    pub fn new(http: Arc<Http>, key: String) -> ComicVine {
        ComicVine {
            http,
            key,
            base: BASE.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> ComicVine {
        self.base = base.into();
        self
    }

    async fn get(&self, path: &str, mut query: Vec<(&str, String)>) -> Result<Value> {
        query.push(("api_key", self.key.clone()));
        query.push(("format", "json".into()));
        let v = self
            .http
            .get_json(&format!("{}{path}", self.base), &query, &[], &["api_key"])
            .await?;
        let code = v["status_code"].as_i64().unwrap_or(0);
        if code != 1 {
            let err = v["error"].as_str().unwrap_or("unknown error");
            bail!("ComicVine: {err}");
        }
        Ok(v)
    }

    fn candidate(v: VolumeJson) -> Candidate {
        // start_year is usually a string, sometimes a number or null.
        let year = match &v.start_year {
            Some(Value::String(s)) => s.trim().parse().ok(),
            Some(Value::Number(n)) => n.as_u64().map(|n| n as u16),
            _ => None,
        };
        let overview = v
            .deck
            .filter(|d| !d.trim().is_empty())
            .or_else(|| v.description.map(|d| strip_html(&d)))
            .filter(|d| !d.is_empty());
        Candidate {
            source: Some(Source::Comicvine),
            id: v.id.to_string(),
            kind: Some(MediaKind::Comic),
            title: v.name.unwrap_or_default(),
            year,
            overview,
            poster: v.image.and_then(|i| i.original_url),
            publisher: v.publisher.and_then(|p| p.name),
            url: v.site_detail_url,
            ..Default::default()
        }
    }
}

#[async_trait]
impl Provider for ComicVine {
    fn source(&self) -> Source {
        Source::Comicvine
    }

    async fn search(&self, query: &str, _year: Option<u16>, _kind: MediaKind) -> Result<Vec<Candidate>> {
        let v = self
            .get(
                "/search/",
                vec![
                    ("query", query.to_string()),
                    ("resources", "volume".into()),
                    ("limit", "10".into()),
                    ("field_list", VOLUME_FIELDS.into()),
                ],
            )
            .await?;
        let list: Vec<VolumeJson> =
            serde_json::from_value(v["results"].clone()).context("ComicVine search")?;
        Ok(list.into_iter().map(Self::candidate).collect())
    }

    async fn lookup(&self, id: &str, _kind: MediaKind) -> Result<Candidate> {
        let v = self
            .get(
                &format!("/volume/4050-{id}/"),
                vec![("field_list", VOLUME_FIELDS.into())],
            )
            .await?;
        let vol: VolumeJson =
            serde_json::from_value(v["results"].clone()).context("ComicVine volume")?;
        Ok(Self::candidate(vol))
    }

    async fn issue(&self, candidate: &Candidate, number: f64) -> Result<Option<Issue>> {
        let n = format_number(number, 0);
        let v = self
            .get(
                "/issues/",
                vec![
                    ("filter", format!("volume:{},issue_number:{n}", candidate.id)),
                    ("field_list", "name,issue_number,cover_date,image,description".into()),
                ],
            )
            .await?;
        let list: Vec<IssueJson> =
            serde_json::from_value(v["results"].clone()).context("ComicVine issues")?;
        Ok(list.into_iter().next().map(|i| Issue {
            number: i.issue_number.unwrap_or(n),
            title: i.name.filter(|t| !t.trim().is_empty()),
            cover_date: i.cover_date,
            cover: i.image.and_then(|i| i.original_url),
            summary: i.description.map(|d| strip_html(&d)).filter(|d| !d.is_empty()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn search_and_issue() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status_code": 1, "error": "OK",
                "results": [{"id": 43277, "name": "Saga", "start_year": "2012",
                             "publisher": {"name": "Image"}, "image": {"original_url": "https://x/s.jpg"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/issues/"))
            .and(query_param("filter", "volume:43277,issue_number:1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status_code": 1, "error": "OK",
                "results": [{"name": "Chapter One", "issue_number": "1", "cover_date": "2012-03-31"}]
            })))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let cv = ComicVine::new(http, "k".into()).with_base_url(server.uri());
        let r = cv.search("Saga", Some(2012), MediaKind::Comic).await.unwrap();
        assert_eq!(r[0].year, Some(2012));
        assert_eq!(r[0].publisher.as_deref(), Some("Image"));
        let i = cv.issue(&r[0], 1.0).await.unwrap().unwrap();
        assert_eq!(i.title.as_deref(), Some("Chapter One"));
    }

    #[tokio::test]
    async fn api_errors_surface() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status_code": 100, "error": "Invalid API Key"
            })))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let cv = ComicVine::new(http, "bad".into()).with_base_url(server.uri());
        let err = cv.search("x", None, MediaKind::Comic).await.unwrap_err();
        assert!(err.to_string().contains("Invalid API Key"));
    }
}
