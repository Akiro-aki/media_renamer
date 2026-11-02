//! MangaDex: manga fallback when AniList has no match. No API key needed.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{
    BookDetails, Candidate, Http, MAX_TAGS, Provider, Role, Source, VolumeCovers, age_rating_for,
    whole_number,
};
use crate::MediaKind;

const BASE: &str = "https://api.mangadex.org";
const COVERS: &str = "https://uploads.mangadex.org/covers";

pub struct Mangadex {
    http: Arc<Http>,
    base: String,
    covers: String,
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
            covers: COVERS.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Mangadex {
        self.base = base.into();
        self
    }

    /// Where cover images are served (`uploads.mangadex.org/covers`).
    pub fn with_covers_url(mut self, covers: impl Into<String>) -> Mangadex {
        self.covers = covers.into();
        self
    }

    /// The manga with its author and artist. `book_details` and
    /// `volume_covers` share this request (and its cache entry).
    async fn manga_with_people(&self, id: &str) -> Result<Value> {
        let q = [
            ("includes[]", "author".to_string()),
            ("includes[]", "artist".to_string()),
        ];
        let v = self
            .http
            .get_json(&format!("{}/manga/{id}", self.base), &q, &[], &[])
            .await?;
        Ok(v["data"].clone())
    }

    /// One cover per volume, in the manga's original language if there is
    /// one, else English, else any. With `chapters`, also which volume each
    /// chapter is in (from `/aggregate`).
    pub async fn volume_covers(&self, id: &str, chapters: bool) -> Result<VolumeCovers> {
        let original = self.manga_with_people(id).await?["attributes"]["originalLanguage"]
            .as_str()
            .map(str::to_string);
        let mut list: Vec<CoverJson> = Vec::new();
        const LIMIT: usize = 100;
        for page in 0..COVER_PAGES {
            let q = [
                ("manga[]", id.to_string()),
                ("order[volume]", "asc".to_string()),
                ("limit", LIMIT.to_string()),
                ("offset", (page * LIMIT).to_string()),
            ];
            let v = self
                .http
                .get_json(&format!("{}/cover", self.base), &q, &[], &[])
                .await?;
            let got: Vec<CoverJson> =
                serde_json::from_value(v["data"].clone()).context("MangaDex covers")?;
            let n = got.len();
            list.extend(got);
            let total = v["total"].as_u64().unwrap_or(0) as usize;
            if n < LIMIT || (page + 1) * LIMIT >= total {
                break;
            }
        }
        let rank = |locale: &Option<String>| match locale.as_deref() {
            l if l.is_some() && l == original.as_deref() => 0,
            Some("en") => 1,
            _ => 2,
        };
        let mut out = VolumeCovers::default();
        let mut best: std::collections::BTreeMap<String, u8> = Default::default();
        for c in list {
            let a = c.attributes;
            let Some(vol) = a.volume.as_deref().and_then(VolumeCovers::key) else { continue };
            let r = rank(&a.locale);
            if best.get(&vol).is_some_and(|&b| b <= r) {
                continue;
            }
            best.insert(vol.clone(), r);
            out.volumes.insert(vol, format!("{}/{id}/{}", self.covers, a.file_name));
        }
        if chapters && !out.volumes.is_empty() {
            let v = self
                .http
                .get_json(&format!("{}/manga/{id}/aggregate", self.base), &[], &[], &[])
                .await?;
            out.chapter_volumes = chapter_volumes(&v["volumes"]);
        }
        Ok(out)
    }

    fn candidate(&self, m: MangaJson) -> Candidate {
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
            poster: cover.map(|f| format!("{}/{}/{f}", self.covers, m.id)),
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
        Ok(list.into_iter().map(|m| self.candidate(m)).collect())
    }

    async fn lookup(&self, id: &str, _kind: MediaKind) -> Result<Candidate> {
        let q = [("includes[]", "cover_art".to_string())];
        let v = self
            .http
            .get_json(&format!("{}/manga/{id}", self.base), &q, &[], &[])
            .await?;
        let m: MangaJson = serde_json::from_value(v["data"].clone()).context("MangaDex lookup")?;
        Ok(self.candidate(m))
    }

    async fn book_details(&self, c: &Candidate) -> Result<BookDetails> {
        let m: DetailsJson =
            serde_json::from_value(self.manga_with_people(&c.id).await?).context("MangaDex details")?;
        Ok(m.into_details())
    }
}

/// Cover pages to read at most (100 covers each).
const COVER_PAGES: usize = 10;

#[derive(Deserialize)]
struct CoverJson {
    attributes: CoverAttributes,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CoverAttributes {
    volume: Option<String>,
    file_name: String,
    locale: Option<String>,
}

/// `/aggregate` volumes → chapter → volume. Uploaders sometimes file a
/// chapter under the wrong volume (Chainsaw Man lists chapter 232 in volume
/// 1, next to chapters 1-7.5). A chapter far from the rest of its volume
/// (beyond 3 median absolute deviations, and beyond the volume's chapter
/// count) is ignored there; one listed in several volumes goes to the
/// closest.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn chapter_volumes(volumes: &Value) -> std::collections::BTreeMap<String, String> {
    // An empty `volumes` is sent as `[]`.
    let Some(volumes) = volumes.as_object() else { return Default::default() };
    let mut claims: std::collections::BTreeMap<String, Vec<(String, f64)>> = Default::default();
    for (name, vol) in volumes {
        let Some(vol_key) = VolumeCovers::key(name) else { continue }; // "none"
        let numbers: Vec<f64> = vol["chapters"]
            .as_object()
            .into_iter()
            .flat_map(|c| c.keys())
            .filter_map(|c| c.parse::<f64>().ok())
            .collect();
        if numbers.is_empty() {
            continue;
        }
        let mid = median(numbers.clone());
        let mad = median(numbers.iter().map(|n| (n - mid).abs()).collect());
        let limit = (3.0 * mad).max(numbers.len() as f64);
        for n in numbers {
            let distance = (n - mid).abs();
            if distance > limit {
                continue;
            }
            let ch = crate::format::template::format_number(n, 0);
            claims.entry(ch).or_default().push((vol_key.clone(), distance));
        }
    }
    claims
        .into_iter()
        .filter_map(|(ch, mut vols)| {
            vols.sort_by(|a, b| a.1.total_cmp(&b.1));
            Some((ch, vols.into_iter().next()?.0))
        })
        .collect()
}

#[derive(Deserialize)]
struct DetailsJson {
    attributes: DetailsAttributes,
    #[serde(default)]
    relationships: Vec<Relationship>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailsAttributes {
    status: Option<String>,
    content_rating: Option<String>,
    #[serde(default)]
    last_volume: Value,
    #[serde(default)]
    last_chapter: Value,
    #[serde(default)]
    tags: Vec<TagJson>,
}

#[derive(Deserialize)]
struct TagJson {
    attributes: TagAttributes,
}

#[derive(Deserialize)]
struct TagAttributes {
    #[serde(default)]
    name: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    group: String,
}

impl DetailsJson {
    fn into_details(self) -> BookDetails {
        let a = self.attributes;
        let mut d = BookDetails::default();
        for t in a.tags {
            let Some(name) = t.attributes.name.get("en").or(t.attributes.name.values().next()) else {
                continue;
            };
            match t.attributes.group.as_str() {
                "genre" => d.genres.push(name.clone()),
                _ if d.tags.len() < MAX_TAGS => d.tags.push(name.clone()),
                _ => {}
            }
        }
        for r in &self.relationships {
            let role = match r.kind.as_str() {
                "author" => Role::Writer,
                "artist" => Role::Penciller,
                _ => continue,
            };
            if let Some(name) = r.attributes.as_ref().and_then(|a| a.get("name")?.as_str()) {
                d.credit(role, name);
            }
        }
        if a.status.as_deref() == Some("completed") {
            d.total_volumes = whole_number(&a.last_volume);
            d.total_chapters = whole_number(&a.last_chapter);
        }
        d.age_rating = a.content_rating.as_deref().and_then(age_rating_for);
        d
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
    async fn volume_covers_prefer_original_language_and_map_chapters() {
        let server = MockServer::start().await;
        let id = "a77742b1-befd-49a4-bff5-1ad4e6b0ef7b";
        // Chainsaw Man, trimmed real responses (2026-11-02).
        for (p, body) in [
            (format!("/manga/{id}"), include_str!("../../tests/fixtures/mangadex_book_details.json")),
            ("/cover".to_string(), include_str!("../../tests/fixtures/mangadex_covers.json")),
            (format!("/manga/{id}/aggregate"), include_str!("../../tests/fixtures/mangadex_aggregate.json")),
        ] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
                .mount(&server)
                .await;
        }
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let m = Mangadex::new(http).with_base_url(server.uri());
        let c = m.volume_covers(id, true).await.unwrap();
        assert_eq!(c.volumes.len(), 24);
        // Volume 1 has an English and a Japanese cover; Japanese is the original.
        let v1 = c.volumes["1"].as_str();
        assert_eq!(
            v1,
            format!("https://uploads.mangadex.org/covers/{id}/59ade418-3590-4d7b-91b0-24c8feaa845f.png")
        );
        assert_eq!(c.for_book(Some(1.0), None), Some(v1));
        assert_eq!(c.for_book(None, Some(5.0)), Some(v1));
        assert_eq!(c.for_book(None, Some(7.5)), Some(v1));
        assert_eq!(c.chapter_volumes.get("8").map(String::as_str), Some("2"));
        // Chapter 232 is filed under volume 1 by mistake: no cover beats a wrong one.
        assert_eq!(c.for_book(None, Some(232.0)), None);
        assert_eq!(c.for_book(None, Some(99999.0)), None);
        assert_eq!(c.for_book(None, None), None);
    }

    #[test]
    fn chapter_volumes_handles_empty_and_none() {
        assert!(chapter_volumes(&json!([])).is_empty());
        let v = json!({
            "none": {"chapters": {"50": {}}},
            "2": {"chapters": {"8": {}, "9": {}, "9.5": {}}}
        });
        let m = chapter_volumes(&v);
        assert_eq!(m.get("9.5").map(String::as_str), Some("2"));
        assert!(!m.contains_key("50"), "chapters without a volume are left out");
    }

    #[tokio::test]
    async fn book_details_tags_creators_and_rating() {
        let server = MockServer::start().await;
        // Chainsaw Man, a trimmed real response (2026-11-02).
        Mock::given(method("GET"))
            .and(path("/manga/a77742b1-befd-49a4-bff5-1ad4e6b0ef7b"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                include_str!("../../tests/fixtures/mangadex_book_details.json"),
                "application/json",
            ))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let m = Mangadex::new(http).with_base_url(server.uri());
        let c = Candidate {
            id: "a77742b1-befd-49a4-bff5-1ad4e6b0ef7b".into(),
            ..Default::default()
        };
        let d = m.book_details(&c).await.unwrap();
        assert_eq!(d.genres, ["Action", "Comedy", "Horror"]);
        assert_eq!(d.tags, ["Award Winning", "Monsters", "Demons", "Gore", "Supernatural"]);
        // One person with both roles.
        assert_eq!(d.names(Role::Writer), ["Fujimoto Tatsuki"]);
        assert_eq!(d.names(Role::Penciller), ["Fujimoto Tatsuki"]);
        // Completed, but lastVolume is "": only the chapter total is known.
        assert_eq!((d.total_volumes, d.total_chapters), (None, Some(232)));
        assert_eq!(d.age_rating, Some("Teen"));
    }

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
