//! MyAnimeList through Jikan (`api.jikan.moe/v4`), an unofficial read-only
//! MAL API. No key needed. Unlike AniList it has episode titles, and like
//! AniList it splits seasons into separate entries.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use super::{Candidate, Episode, Http, Provider, SeriesInfo, Source, year_of};
use crate::MediaKind;
use crate::config::TitlePreference;

const BASE: &str = "https://api.jikan.moe/v4";
/// Jikan pages episode lists by 100. Stop well past any real show.
const MAX_EPISODE_PAGES: u32 = 40;

pub struct Mal {
    http: Arc<Http>,
    title_pref: TitlePreference,
    base: String,
}

/// An anime or manga entry (`/anime`, `/manga`, `/anime/{id}`, `/manga/{id}`).
#[derive(Deserialize)]
struct EntryJson {
    mal_id: u64,
    url: Option<String>,
    #[serde(default)]
    titles: Vec<TitleJson>,
    /// Deprecated in favour of `titles`, kept as a fallback.
    title: Option<String>,
    title_english: Option<String>,
    title_japanese: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    synopsis: Option<String>,
    /// Anime season year.
    year: Option<u16>,
    aired: Option<DateRange>,
    published: Option<DateRange>,
    images: Option<Images>,
    #[serde(default)]
    genres: Vec<Named>,
}

#[derive(Deserialize)]
struct TitleJson {
    #[serde(rename = "type")]
    kind: String,
    title: String,
}

#[derive(Deserialize)]
struct DateRange {
    from: Option<String>,
}

#[derive(Deserialize)]
struct Images {
    jpg: Option<ImageUrls>,
}

#[derive(Deserialize)]
struct ImageUrls {
    image_url: Option<String>,
    large_image_url: Option<String>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct EpisodePage {
    #[serde(default)]
    data: Vec<EpisodeJson>,
    pagination: Option<Pagination>,
}

#[derive(Deserialize)]
struct Pagination {
    #[serde(default)]
    has_next_page: bool,
}

#[derive(Deserialize)]
struct EpisodeJson {
    /// The episode number (MAL's episode-number column), not a global id.
    mal_id: u32,
    title: Option<String>,
    aired: Option<String>,
}

impl Mal {
    pub fn new(http: Arc<Http>, title_pref: TitlePreference) -> Mal {
        Mal {
            http,
            title_pref,
            base: BASE.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Mal {
        self.base = base.into();
        self
    }

    fn path(kind: MediaKind) -> &'static str {
        if kind == MediaKind::Manga { "manga" } else { "anime" }
    }

    fn candidate(&self, e: EntryJson, kind: MediaKind) -> Candidate {
        let of_type = |t: &str| {
            e.titles
                .iter()
                .find(|x| x.kind.eq_ignore_ascii_case(t))
                .map(|x| x.title.clone())
                .filter(|s| !s.trim().is_empty())
        };
        let romaji = of_type("Default").or_else(|| e.title.clone());
        let english = of_type("English").or_else(|| e.title_english.clone());
        let native = of_type("Japanese").or_else(|| e.title_japanese.clone());
        let title = match self.title_pref {
            TitlePreference::Romaji => romaji.clone(),
            TitlePreference::English => english.clone().or(romaji.clone()),
            TitlePreference::Native => native.or(romaji.clone()),
        }
        .unwrap_or_default();

        let mut alt: Vec<String> = Vec::new();
        for t in e
            .titles
            .iter()
            .map(|t| &t.title)
            .chain(e.title.iter())
            .chain(e.title_english.iter())
            .chain(e.title_japanese.iter())
        {
            if *t != title && !t.trim().is_empty() && !alt.contains(t) {
                alt.push(t.clone());
            }
        }

        let started = e.aired.as_ref().or(e.published.as_ref()).and_then(|d| d.from.as_deref());
        Candidate {
            source: Some(Source::Mal),
            id: e.mal_id.to_string(),
            kind: Some(kind),
            title,
            alt_titles: alt,
            year: e.year.or_else(|| year_of(started)),
            overview: e.synopsis.as_deref().map(clean_synopsis).filter(|s| !s.is_empty()),
            poster: e
                .images
                .and_then(|i| i.jpg)
                .and_then(|j| j.large_image_url.or(j.image_url)),
            genres: e.genres.into_iter().map(|g| g.name).collect(),
            english_title: english,
            url: e.url,
            ..Default::default()
        }
    }

    /// The episode list of MAL anime `id`, following Jikan's pages.
    pub async fn episodes(&self, id: &str) -> Result<SeriesInfo> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            bail!("MAL ids are numbers, got '{id}'");
        }
        let mut episodes = Vec::new();
        for page in 1..=MAX_EPISODE_PAGES {
            let v = self
                .http
                .get_json(
                    &format!("{}/anime/{id}/episodes", self.base),
                    &[("page", page.to_string())],
                    &[],
                    &[],
                )
                .await?;
            let p: EpisodePage = serde_json::from_value(v).context("MAL episodes")?;
            episodes.extend(p.data.into_iter().map(|e| Episode {
                // A MAL entry is one season, numbered from 1 (see `single_entry`).
                season: 1,
                episode: e.mal_id,
                absolute: Some(e.mal_id),
                title: e.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
                air_date: e.aired.and_then(|d| d.get(..10).map(String::from)),
                overview: None,
            }));
            if !p.pagination.is_some_and(|p| p.has_next_page) {
                break;
            }
        }
        Ok(SeriesInfo {
            episodes,
            single_entry: true,
            ..Default::default()
        })
    }
}

/// Drop MAL's attribution line ("[Written by MAL Rewrite]") and tidy spaces.
fn clean_synopsis(s: &str) -> String {
    s.replace("[Written by MAL Rewrite]", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[async_trait]
impl Provider for Mal {
    fn source(&self) -> Source {
        Source::Mal
    }

    async fn search(&self, query: &str, _year: Option<u16>, kind: MediaKind) -> Result<Vec<Candidate>> {
        let v = self
            .http
            .get_json(
                &format!("{}/{}", self.base, Self::path(kind)),
                &[("q", query.to_string()), ("limit", "10".to_string())],
                &[],
                &[],
            )
            .await?;
        let list: Vec<EntryJson> = serde_json::from_value(v["data"].clone()).context("MAL search")?;
        let mut seen = std::collections::HashSet::new();
        Ok(list
            .into_iter()
            // Jikan can repeat an entry across a result page.
            .filter(|e| seen.insert(e.mal_id))
            // Like AniList: novels share the manga's title and would win the
            // match. `--id mal:N` still reaches them.
            .filter(|e| !matches!(e.kind.as_deref(), Some("Novel" | "Light Novel")))
            .map(|e| self.candidate(e, kind))
            .collect())
    }

    async fn lookup(&self, id: &str, kind: MediaKind) -> Result<Candidate> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            bail!("MAL ids are numbers, got '{id}'");
        }
        let v = self
            .http
            .get_json(&format!("{}/{}/{id}", self.base, Self::path(kind)), &[], &[], &[])
            .await?;
        let e: EntryJson = serde_json::from_value(v["data"].clone()).context("MAL lookup")?;
        Ok(self.candidate(e, kind))
    }

    /// Episodes of an anime matched on MAL, or of an entry from another
    /// source that knows its MAL id (AniList's `idMal`).
    async fn series(&self, candidate: &Candidate) -> Result<SeriesInfo> {
        match candidate.id_for(Source::Mal) {
            Some(id) => self.episodes(id).await,
            None => Ok(SeriesInfo::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Shapes follow Jikan's OpenAPI spec (jikan-me/jikan-rest,
    /// storage/api-docs/api-docs.json); the API wasn't reachable when these
    /// were written.
    fn frieren() -> Value {
        json!({
            "mal_id": 52991,
            "url": "https://myanimelist.net/anime/52991/Sousou_no_Frieren",
            "images": {"jpg": {"image_url": "https://cdn.myanimelist.net/images/anime/1015/138006.jpg",
                               "large_image_url": "https://cdn.myanimelist.net/images/anime/1015/138006l.jpg"}},
            "titles": [
                {"type": "Default", "title": "Sousou no Frieren"},
                {"type": "Synonym", "title": "Frieren at the Funeral"},
                {"type": "Japanese", "title": "葬送のフリーレン"},
                {"type": "English", "title": "Frieren: Beyond Journey's End"}
            ],
            "title": "Sousou no Frieren",
            "title_english": "Frieren: Beyond Journey's End",
            "title_japanese": "葬送のフリーレン",
            "type": "TV",
            "episodes": 28,
            "aired": {"from": "2023-09-29T00:00:00+00:00", "to": "2024-03-22T00:00:00+00:00"},
            "synopsis": "During their decade-long quest...\n\n[Written by MAL Rewrite]",
            "year": 2023,
            "genres": [{"mal_id": 2, "type": "anime", "name": "Adventure"}, {"mal_id": 8, "type": "anime", "name": "Drama"}]
        })
    }

    fn mal(server: &MockServer, pref: TitlePreference) -> Mal {
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        Mal::new(http, pref).with_base_url(server.uri())
    }

    #[tokio::test]
    async fn search_and_lookup() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/anime"))
            .and(query_param("q", "Frieren"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "pagination": {"last_visible_page": 1, "has_next_page": false},
                "data": [frieren(), frieren()]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/anime/52991"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": frieren()})))
            .mount(&server)
            .await;

        let r = mal(&server, TitlePreference::Romaji)
            .search("Frieren", None, MediaKind::Anime)
            .await
            .unwrap();
        assert_eq!(r.len(), 1, "duplicates are dropped");
        let c = &r[0];
        assert_eq!((c.source(), c.id.as_str()), (Source::Mal, "52991"));
        assert_eq!(c.title, "Sousou no Frieren");
        assert_eq!(c.english_title.as_deref(), Some("Frieren: Beyond Journey's End"));
        assert!(c.alt_titles.contains(&"Frieren at the Funeral".to_string()));
        assert!(!c.alt_titles.contains(&c.title));
        assert_eq!(c.year, Some(2023));
        assert_eq!(c.overview.as_deref(), Some("During their decade-long quest..."));
        assert!(c.poster.as_deref().unwrap().ends_with("138006l.jpg"));
        assert_eq!(c.genres, ["Adventure", "Drama"]);

        let c = mal(&server, TitlePreference::English)
            .lookup("52991", MediaKind::Anime)
            .await
            .unwrap();
        assert_eq!(c.title, "Frieren: Beyond Journey's End");
        assert!(mal(&server, TitlePreference::Romaji).lookup("abc", MediaKind::Anime).await.is_err());
    }

    #[tokio::test]
    async fn manga_search_drops_novels() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manga"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"mal_id": 1, "type": "Light Novel", "titles": [{"type": "Default", "title": "Spy Kyoushitsu"}],
                 "published": {"from": "2020-01-18T00:00:00+00:00"}},
                {"mal_id": 2, "type": "Manga", "titles": [{"type": "Default", "title": "Spy Kyoushitsu"}],
                 "published": {"from": "2021-01-01T00:00:00+00:00"}}
            ]})))
            .mount(&server)
            .await;
        let r = mal(&server, TitlePreference::Romaji)
            .search("Spy Kyoushitsu", None, MediaKind::Manga)
            .await
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].id.as_str(), r[0].year), ("2", Some(2021)));
    }

    #[tokio::test]
    async fn episodes_follow_pages() {
        let server = MockServer::start().await;
        let page = |range: std::ops::RangeInclusive<u32>, next: bool| {
            let data: Vec<Value> = range
                .map(|n| json!({
                    "mal_id": n,
                    "title": if n == 12 { "A Real Hero".to_string() } else if n == 101 { String::new() } else { format!("Ep {n}") },
                    "aired": "2023-12-15T00:00:00+00:00",
                    "filler": false, "recap": false
                }))
                .collect();
            json!({"pagination": {"last_visible_page": 2, "has_next_page": next}, "data": data})
        };
        Mock::given(method("GET"))
            .and(path("/anime/52991/episodes"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(1..=100, true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/anime/52991/episodes"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(101..=102, false)))
            .mount(&server)
            .await;

        // An AniList match reaches the list through its MAL id.
        let c = Candidate {
            source: Some(Source::Anilist),
            id: "154587".into(),
            ext_ids: [(Source::Mal, "52991".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let s = mal(&server, TitlePreference::Romaji).series(&c).await.unwrap();
        assert!(s.single_entry);
        assert_eq!(s.episodes.len(), 102);
        let e = s.by_absolute(12).unwrap();
        assert_eq!((e.season, e.episode), (1, 12));
        assert_eq!(e.title.as_deref(), Some("A Real Hero"));
        assert_eq!(e.air_date.as_deref(), Some("2023-12-15"));
        assert_eq!(s.by_absolute(101).unwrap().title, None, "empty titles are dropped");

        // No MAL id: nothing to fetch.
        let other = Candidate { source: Some(Source::Anilist), id: "1".into(), ..Default::default() };
        assert!(mal(&server, TitlePreference::Romaji).series(&other).await.unwrap().episodes.is_empty());
    }
}
