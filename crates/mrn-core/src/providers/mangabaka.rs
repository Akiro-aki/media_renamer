//! MangaBaka (`api.mangabaka.org/v1`): manga, manhwa and manhua. No API key.
//!
//! One MangaBaka series carries the ids it was built from (AniList, MAL,
//! MangaUpdates, Kitsu, ...); the AniList and MAL ones go to `ext_ids`.
//! It has no MangaDex ids.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{
    BookDetails, Candidate, Http, MAX_TAGS, Provider, Role, Source, age_rating_for, whole_number, year_of,
};
use crate::MediaKind;
use crate::config::TitlePreference;

const BASE: &str = "https://api.mangabaka.org/v1";
/// Shown instead of a title when MangaBaka has none yet.
const PLACEHOLDER: &str = "unknown title";

pub struct Mangabaka {
    http: Arc<Http>,
    base: String,
    title_pref: TitlePreference,
}

#[derive(Deserialize)]
struct SeriesJson {
    id: u64,
    #[serde(default)]
    state: Option<String>,
    merged_with: Option<u64>,
    #[serde(rename = "type")]
    kind: Option<String>,
    title: Option<String>,
    native_title: Option<String>,
    romanized_title: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    titles: Vec<TitleJson>,
    original_language: Option<String>,
    description: Option<String>,
    year: Option<u16>,
    #[serde(default)]
    published: Option<Published>,
    #[serde(default)]
    cover: Option<Cover>,
    #[serde(default, deserialize_with = "null_as_empty")]
    genres: Vec<String>,
    canonical_url: Option<String>,
    /// Ids on the sites this series was built from.
    #[serde(default)]
    source: Option<std::collections::BTreeMap<String, Value>>,
    #[serde(default, deserialize_with = "null_as_empty")]
    authors: Vec<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    artists: Vec<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    tags_v2: Vec<TagJson>,
    status: Option<String>,
    content_rating: Option<String>,
    #[serde(default)]
    final_volume: Value,
    #[serde(default)]
    total_chapters: Value,
}

#[derive(Deserialize)]
struct TagJson {
    name: String,
    /// `core`, `defining`, `recurrent`, `incidental` or `unweighted`.
    #[serde(default)]
    weight: Option<String>,
    #[serde(default)]
    is_spoiler: bool,
    #[serde(default)]
    is_genre: bool,
}

/// MangaBaka sends `null` for some empty lists (`genres`, `tags`).
fn null_as_empty<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct TitleJson {
    language: String,
    title: String,
    #[serde(default)]
    is_primary: bool,
}

#[derive(Deserialize)]
struct Published {
    start_date: Option<String>,
}

#[derive(Deserialize)]
struct Cover {
    raw: Option<CoverImage>,
}

#[derive(Deserialize)]
struct CoverImage {
    url: Option<String>,
}

impl SeriesJson {
    fn is_novel(&self) -> bool {
        self.kind.as_deref() == Some("novel")
    }

    /// The first primary title in `language`, else any title in it.
    fn title_in(&self, language: &str) -> Option<String> {
        let mut in_lang = self
            .titles
            .iter()
            .filter(|t| t.language.eq_ignore_ascii_case(language) && usable(&t.title));
        let first = in_lang.next()?;
        Some(
            std::iter::once(first)
                .chain(in_lang)
                .find(|t| t.is_primary)
                .unwrap_or(first)
                .title
                .clone(),
        )
    }
}

fn usable(title: &str) -> bool {
    let t = title.trim();
    !t.is_empty() && !t.to_ascii_lowercase().starts_with(PLACEHOLDER)
}

fn some_usable(t: &Option<String>) -> Option<String> {
    t.as_deref().filter(|t| usable(t)).map(str::to_string)
}

/// `slice_of_life` → `Slice of Life`, `sci-fi` → `Sci-Fi` (MangaBaka's own labels).
fn genre_label(value: &str) -> String {
    value
        .split('_')
        .enumerate()
        .map(|(i, word)| {
            if i > 0 && word == "of" {
                return word.to_string();
            }
            word.split('-')
                .map(|part| {
                    let mut c = part.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect())
                        .unwrap_or_default()
                })
                .collect::<Vec<String>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Mangabaka {
    pub fn new(http: Arc<Http>, title_pref: TitlePreference) -> Mangabaka {
        Mangabaka {
            http,
            base: BASE.to_string(),
            title_pref,
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Mangabaka {
        self.base = base.into();
        self
    }

    fn candidate(&self, s: SeriesJson) -> Candidate {
        let main = some_usable(&s.title);
        // Romaji: the primary title in the original language's Latin script
        // (`ja-Latn`). `romanized_title` is a fallback only: it is sometimes
        // a translation.
        let romaji = s
            .original_language
            .as_deref()
            .and_then(|l| s.title_in(&format!("{l}-Latn")))
            .or_else(|| some_usable(&s.romanized_title));
        let english = s.title_in("en").or_else(|| main.clone());
        let native = some_usable(&s.native_title);
        let title = match self.title_pref {
            TitlePreference::Romaji => romaji.clone().or(main.clone()),
            TitlePreference::English => english.clone().or(romaji.clone()),
            TitlePreference::Native => native.clone().or(romaji.clone()).or(main.clone()),
        }
        .or_else(|| english.clone())
        .unwrap_or_default();

        let mut alt: Vec<String> = Vec::new();
        for t in main
            .iter()
            .chain(romaji.iter())
            .chain(native.iter())
            .chain(s.titles.iter().map(|t| &t.title))
        {
            if usable(t) && *t != title && !alt.contains(t) {
                alt.push(t.clone());
            }
        }

        let ext_ids = s
            .source
            .iter()
            .flatten()
            .filter_map(|(site, v)| {
                let source = match site.as_str() {
                    "anilist" => Source::Anilist,
                    "my_anime_list" => Source::Mal,
                    _ => return None,
                };
                let id = match v.get("id")? {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) => s.trim().to_string(),
                    _ => return None,
                };
                (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then_some((source, id))
            })
            .collect();

        let year = s.year.or_else(|| {
            year_of(s.published.as_ref().and_then(|p| p.start_date.as_deref()))
        });
        Candidate {
            source: Some(Source::Mangabaka),
            kind: Some(MediaKind::Manga),
            english_title: english.filter(|e| *e != title),
            title,
            alt_titles: alt,
            year,
            overview: s.description.as_deref().map(str::trim).filter(|d| !d.is_empty()).map(String::from),
            poster: s.cover.and_then(|c| c.raw).and_then(|r| r.url),
            genres: s.genres.iter().map(|g| genre_label(g)).collect(),
            url: s
                .canonical_url
                .or_else(|| Some(format!("https://mangabaka.org/{}", s.id))),
            ext_ids,
            id: s.id.to_string(),
            ..Default::default()
        }
    }

    /// Tags, creators and totals from a series.
    fn details(s: &SeriesJson) -> BookDetails {
        let mut d = BookDetails {
            // Series have 50-150 tags; `core` ones describe it best. Genres
            // are repeated as tags (`is_genre`) and already in `genres`.
            tags: s
                .tags_v2
                .iter()
                .filter(|t| t.weight.as_deref() == Some("core") && !t.is_spoiler && !t.is_genre)
                .take(MAX_TAGS)
                .map(|t| t.name.clone())
                .collect(),
            genres: s.genres.iter().map(|g| genre_label(g)).collect(),
            age_rating: s.content_rating.as_deref().and_then(age_rating_for),
            ..Default::default()
        };
        for a in &s.authors {
            d.credit(Role::Writer, a);
        }
        for a in &s.artists {
            d.credit(Role::Penciller, a);
        }
        if s.status.as_deref() == Some("completed") {
            d.total_volumes = whole_number(&s.final_volume);
            d.total_chapters = whole_number(&s.total_chapters);
        }
        d
    }

    /// A series by id, following a merge once.
    async fn fetch(&self, id: &str) -> Result<SeriesJson> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            bail!("MangaBaka ids are numbers, got '{id}'");
        }
        let mut s = self.get_series(id).await?;
        // A merged series points at the one that replaced it.
        if s.state.as_deref() == Some("merged")
            && let Some(new_id) = s.merged_with
        {
            s = self.get_series(&new_id.to_string()).await?;
        }
        if s.state.as_deref() == Some("deleted") {
            bail!("MangaBaka series {id} was deleted");
        }
        Ok(s)
    }

    async fn get_series(&self, id: &str) -> Result<SeriesJson> {
        let v = self
            .http
            .get_json(&format!("{}/series/{id}", self.base), &[], &[], &[])
            .await?;
        serde_json::from_value(v["data"].clone()).context("MangaBaka lookup")
    }
}

#[async_trait]
impl Provider for Mangabaka {
    fn source(&self) -> Source {
        Source::Mangabaka
    }

    async fn search(&self, query: &str, _year: Option<u16>, _kind: MediaKind) -> Result<Vec<Candidate>> {
        let q = [
            ("q", query.to_string()),
            ("type_not", "novel".to_string()),
            ("limit", "10".to_string()),
        ];
        let v = self
            .http
            .get_json(&format!("{}/series/search", self.base), &q, &[], &[])
            .await?;
        let list: Vec<SeriesJson> =
            serde_json::from_value(v["data"].clone()).context("MangaBaka search")?;
        Ok(list
            .into_iter()
            .filter(|s| !s.is_novel() && s.state.as_deref().is_none_or(|st| st == "active"))
            .map(|s| self.candidate(s))
            .collect())
    }

    async fn lookup(&self, id: &str, _kind: MediaKind) -> Result<Candidate> {
        let s = self.fetch(id).await?;
        Ok(self.candidate(s))
    }

    async fn book_details(&self, c: &Candidate) -> Result<BookDetails> {
        Ok(Self::details(&self.fetch(&c.id).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Trimmed real responses (api.mangabaka.org, 2026-11-02). Data: MangaBaka
    // and its sources, CC BY-NC-SA 4.0.
    const SEARCH: &str = include_str!("../../tests/fixtures/mangabaka_search.json");
    const SERIES: &str = include_str!("../../tests/fixtures/mangabaka_series.json");

    fn provider(server: &MockServer, pref: TitlePreference) -> Mangabaka {
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        Mangabaka::new(http, pref).with_base_url(server.uri())
    }

    #[tokio::test]
    async fn search_drops_novels_and_reads_ids() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/series/search"))
            .and(query_param("q", "solo leveling"))
            .and(query_param("type_not", "novel"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(SEARCH, "application/json"))
            .mount(&server)
            .await;
        let r = provider(&server, TitlePreference::Romaji)
            .search("solo leveling", None, MediaKind::Manga)
            .await
            .unwrap();
        // The fixture holds the 2016 novel too; it is filtered out here.
        let ids: Vec<&str> = r.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["3397", "1252"]);
        let c = &r[0];
        assert_eq!(c.title, "Na Honjaman Level Up");
        assert_eq!(c.english_title.as_deref(), Some("Solo Leveling"));
        assert!(c.alt_titles.contains(&"Solo Leveling".to_string()));
        assert!(c.alt_titles.contains(&"나 혼자만 레벨업".to_string()));
        assert_eq!(c.year, Some(2018));
        assert_eq!(c.id_for(Source::Anilist), Some("105398"));
        assert_eq!(c.id_for(Source::Mal), Some("121496"));
        assert_eq!(c.id_for(Source::Mangabaka), Some("3397"));
        assert_eq!(c.ext_ids.len(), 2);
        assert_eq!(c.url.as_deref(), Some("https://mangabaka.org/manhwa/3397/Solo-Leveling"));
        assert!(c.poster.as_deref().unwrap().starts_with("https://images.mangabaka.dev/"));
    }

    #[tokio::test]
    async fn lookup_follows_title_preference_and_genres() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/series/1995"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(SERIES, "application/json"))
            .mount(&server)
            .await;
        let get = |pref| {
            let p = provider(&server, pref);
            async move { p.lookup("1995", MediaKind::Manga).await.unwrap() }
        };
        // `romanized_title` is "Frieren Cenazede" (Turkish); the ja-Latn title wins.
        let c = get(TitlePreference::Romaji).await;
        assert_eq!(c.title, "Sousou no Frieren");
        assert_eq!(c.english_title.as_deref(), Some("Frieren: Beyond Journey's End"));
        assert!(c.genres.contains(&"Slice of Life".to_string()), "{:?}", c.genres);
        assert!(c.genres.contains(&"Award Winning".to_string()));
        assert_eq!(c.id_for(Source::Mal), Some("126287"));
        assert_eq!(get(TitlePreference::English).await.title, "Frieren: Beyond Journey's End");
        assert_eq!(get(TitlePreference::Native).await.title, "葬送のフリーレン");
    }

    #[tokio::test]
    async fn lookup_follows_merges_and_skips_placeholders() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/series/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "id": 10, "state": "merged", "merged_with": 20, "title": "Old"
            }})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/series/20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "id": 20, "state": "active", "type": "manga",
                "title": "unknown title (please report on Discord)",
                "romanized_title": "Sousou no Frieren Koushiki Fan Book",
                "native_title": null, "genres": null, "titles": null,
                "source": {"anilist": {"id": null}, "my_anime_list": {"id": null}},
                "published": {"start_date": "2023-04-17"}
            }})))
            .mount(&server)
            .await;
        let c = provider(&server, TitlePreference::English)
            .lookup("10", MediaKind::Manga)
            .await
            .unwrap();
        assert_eq!(c.id, "20");
        assert_eq!(c.title, "Sousou no Frieren Koushiki Fan Book");
        assert!(c.alt_titles.is_empty(), "{:?}", c.alt_titles);
        assert!(c.ext_ids.is_empty());
        assert_eq!(c.year, Some(2023));
        assert_eq!(c.url.as_deref(), Some("https://mangabaka.org/20"));

        let err = provider(&server, TitlePreference::Romaji)
            .lookup("abc", MediaKind::Manga)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("numbers"), "{err}");
    }

    #[tokio::test]
    async fn book_details_core_tags_creators_and_counts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/series/1995"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(SERIES, "application/json"))
            .mount(&server)
            .await;
        let p = provider(&server, TitlePreference::Romaji);
        let c = p.lookup("1995", MediaKind::Manga).await.unwrap();
        let d = p.book_details(&c).await.unwrap();
        assert_eq!(&d.tags[..3], ["Travel", "Female Lead", "Calm Female Lead"]);
        assert!(d.tags.len() <= MAX_TAGS);
        assert!(!d.tags.iter().any(|t| t == "Fantasy"), "genre tags are left to Genre");
        assert!(d.genres.contains(&"Slice of Life".to_string()));
        assert_eq!(d.names(Role::Writer), ["Kanehito Yamada"]);
        assert_eq!(d.names(Role::Penciller), ["Tsukasa Abe"]);
        // The fixture's status is set to completed.
        assert_eq!((d.total_volumes, d.total_chapters), (Some(15), Some(147)));
        assert_eq!(d.age_rating, Some("Everyone"));
    }

    #[test]
    fn genre_labels_match_mangabaka() {
        for (value, label) in [
            ("action", "Action"),
            ("slice_of_life", "Slice of Life"),
            ("sci-fi", "Sci-Fi"),
            ("mahou_shoujo", "Mahou Shoujo"),
        ] {
            assert_eq!(genre_label(value), label);
        }
    }
}
