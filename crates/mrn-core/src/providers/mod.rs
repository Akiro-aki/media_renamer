//! Metadata providers: TMDB, AniList, MAL (through Jikan), MangaDex,
//! ComicVine. MangaBaka is a known source; its provider comes later.

pub mod anilist;
pub mod comicvine;
pub mod http;
pub mod mal;
pub mod mangadex;
pub mod tmdb;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::MediaKind;
use crate::config::{Config, ProviderOrder};
pub use http::Http;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Tmdb,
    Anilist,
    #[serde(alias = "myanimelist")]
    Mal,
    Mangadex,
    #[serde(alias = "mb")]
    Mangabaka,
    #[serde(alias = "cv")]
    Comicvine,
    /// No lookup: names come from the parsed filename (`--offline`).
    Local,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Tmdb => "tmdb",
            Source::Anilist => "anilist",
            Source::Mal => "mal",
            Source::Mangadex => "mangadex",
            Source::Mangabaka => "mangabaka",
            Source::Comicvine => "comicvine",
            Source::Local => "local",
        }
    }

    /// Whether this source can be searched for `kind`.
    pub fn searches(self, kind: MediaKind) -> bool {
        use MediaKind::*;
        match self {
            Source::Tmdb => matches!(kind, Movie | Tv),
            Source::Anilist | Source::Mal => matches!(kind, Anime | Manga),
            Source::Mangadex | Source::Mangabaka => kind == Manga,
            Source::Comicvine => kind == Comic,
            Source::Local => false,
        }
    }

    /// Whether this source can supply anime episode titles.
    pub fn lists_anime_episodes(self) -> bool {
        matches!(self, Source::Tmdb | Source::Mal)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Source {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "tmdb" => Ok(Source::Tmdb),
            "anilist" => Ok(Source::Anilist),
            "mal" | "myanimelist" => Ok(Source::Mal),
            "mangadex" => Ok(Source::Mangadex),
            "mangabaka" | "mb" => Ok(Source::Mangabaka),
            "comicvine" | "cv" => Ok(Source::Comicvine),
            o => Err(format!(
                "unknown source '{o}' (tmdb, anilist, mal, mangadex, mangabaka, comicvine)"
            )),
        }
    }
}

/// `--id tmdb:1399`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedId {
    pub source: Source,
    pub id: String,
}

impl FromStr for ForcedId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (src, id) = s
            .split_once(':')
            .ok_or_else(|| format!("expected SOURCE:ID (e.g. tmdb:1399), got '{s}'"))?;
        if id.trim().is_empty() {
            return Err(format!("missing id in '{s}'"));
        }
        Ok(ForcedId {
            source: src.parse()?,
            id: id.trim().to_string(),
        })
    }
}

/// A search result: a movie, series, manga or comic series.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Candidate {
    pub source: Option<Source>,
    pub id: String,
    pub kind: Option<MediaKind>,
    /// Title used for naming.
    pub title: String,
    /// Other titles used for matching (original, romaji, synonyms).
    pub alt_titles: Vec<String>,
    pub year: Option<u16>,
    pub overview: Option<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub publisher: Option<String>,
    pub genres: Vec<String>,
    /// English title, if different (used to cross-reference TMDB for anime).
    pub english_title: Option<String>,
    pub url: Option<String>,
    /// The same entry on other sites (e.g. AniList's `idMal`).
    pub ext_ids: BTreeMap<Source, String>,
}

impl Candidate {
    pub fn source(&self) -> Source {
        self.source.unwrap_or(Source::Local)
    }

    /// This entry's id on `source`: its own id if it came from there,
    /// otherwise a cross-site id.
    pub fn id_for(&self, source: Source) -> Option<&str> {
        if self.source() == source && !self.id.is_empty() {
            Some(&self.id)
        } else {
            self.ext_ids.get(&source).map(String::as_str)
        }
    }

    /// `Title (Year)` for display.
    pub fn label(&self) -> String {
        match self.year {
            Some(y) => format!("{} ({y})", self.title),
            None => self.title.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Episode {
    pub season: u32,
    pub episode: u32,
    /// Position counting all regular seasons in order (anime numbering).
    pub absolute: Option<u32>,
    pub title: Option<String>,
    pub air_date: Option<String>,
    pub overview: Option<String>,
}

/// Episode list and per-season art for a series.
#[derive(Debug, Clone, Default)]
pub struct SeriesInfo {
    pub episodes: Vec<Episode>,
    pub season_posters: BTreeMap<u32, String>,
    /// The list covers one AniList/MAL-style entry: a single season, numbered
    /// from 1. Only files without a season or with season 1 use it.
    pub single_entry: bool,
}

impl SeriesInfo {
    pub fn by_season_episode(&self, season: u32, episode: u32) -> Option<&Episode> {
        self.episodes
            .iter()
            .find(|e| e.season == season && e.episode == episode)
    }

    pub fn by_absolute(&self, absolute: u32) -> Option<&Episode> {
        self.episodes.iter().find(|e| e.absolute == Some(absolute))
    }

    pub fn by_date(&self, date: &str) -> Option<&Episode> {
        self.episodes
            .iter()
            .find(|e| e.air_date.as_deref() == Some(date))
    }
}

/// One comic issue.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Issue {
    pub number: String,
    pub title: Option<String>,
    pub cover_date: Option<String>,
    pub cover: Option<String>,
    pub summary: Option<String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn source(&self) -> Source;

    async fn search(&self, query: &str, year: Option<u16>, kind: MediaKind) -> Result<Vec<Candidate>>;

    async fn lookup(&self, id: &str, kind: MediaKind) -> Result<Candidate>;

    /// Episodes of a series. Empty for providers without episode data.
    async fn series(&self, _candidate: &Candidate) -> Result<SeriesInfo> {
        Ok(SeriesInfo::default())
    }

    /// A comic issue by number.
    async fn issue(&self, _candidate: &Candidate, _number: f64) -> Result<Option<Issue>> {
        Ok(None)
    }
}

/// All configured providers.
#[derive(Clone)]
pub struct Providers {
    pub tmdb: Option<Arc<tmdb::Tmdb>>,
    pub anilist: Arc<anilist::Anilist>,
    pub mal: Arc<mal::Mal>,
    pub mangadex: Arc<mangadex::Mangadex>,
    pub comicvine: Option<Arc<comicvine::ComicVine>>,
}

impl Providers {
    pub fn new(config: &Config, http: Arc<Http>) -> Providers {
        Providers {
            tmdb: config.api.tmdb.as_ref().map(|key| {
                Arc::new(tmdb::Tmdb::new(
                    http.clone(),
                    key.clone(),
                    config.general.language.clone(),
                ))
            }),
            anilist: Arc::new(anilist::Anilist::new(
                http.clone(),
                config.general.anime_title,
            )),
            mal: Arc::new(mal::Mal::new(http.clone(), config.general.anime_title)),
            mangadex: Arc::new(mangadex::Mangadex::new(http.clone())),
            comicvine: config
                .api
                .comicvine
                .as_ref()
                .map(|key| Arc::new(comicvine::ComicVine::new(http.clone(), key.clone()))),
        }
    }

    /// The provider for `source`, if it is available (keyed sources need their key).
    pub fn get(&self, source: Source) -> Option<Arc<dyn Provider>> {
        match source {
            Source::Tmdb => self.tmdb.clone().map(|p| p as Arc<dyn Provider>),
            Source::Anilist => Some(self.anilist.clone()),
            Source::Mal => Some(self.mal.clone()),
            Source::Mangadex => Some(self.mangadex.clone()),
            Source::Comicvine => self.comicvine.clone().map(|p| p as Arc<dyn Provider>),
            Source::Mangabaka | Source::Local => None,
        }
    }

    /// Providers to search for a kind, in priority order. Anime and manga
    /// follow `order`; unavailable sources are skipped.
    pub fn for_kind(&self, kind: MediaKind, order: &ProviderOrder) -> Vec<Arc<dyn Provider>> {
        let sources: &[Source] = match kind {
            MediaKind::Movie | MediaKind::Tv => &[Source::Tmdb],
            MediaKind::Comic => &[Source::Comicvine],
            MediaKind::Anime => &order.anime,
            MediaKind::Manga => &order.manga,
        };
        sources.iter().filter_map(|&s| self.get(s)).collect()
    }

    pub fn by_source(&self, source: Source) -> Result<Arc<dyn Provider>> {
        Ok(match source {
            Source::Tmdb => match &self.tmdb {
                Some(t) => t.clone(),
                None => bail!("TMDB needs an API key: set TMDB_API_KEY or [api] tmdb in the config"),
            },
            Source::Anilist => self.anilist.clone(),
            Source::Mangadex => self.mangadex.clone(),
            Source::Mal => self.mal.clone(),
            Source::Mangabaka => bail!("{source} is not supported yet"),
            Source::Comicvine => match &self.comicvine {
                Some(c) => c.clone(),
                None => bail!(
                    "ComicVine needs an API key: set COMICVINE_API_KEY or [api] comicvine in the config"
                ),
            },
            Source::Local => bail!("'local' is not a lookup source"),
        })
    }

    /// Why a kind has no provider, for a helpful message.
    pub fn missing_reason(&self, kind: MediaKind) -> Option<&'static str> {
        match kind {
            MediaKind::Movie | MediaKind::Tv if self.tmdb.is_none() => Some(
                "movies and TV need a TMDB API key (TMDB_API_KEY or [api] tmdb); free at https://www.themoviedb.org/settings/api",
            ),
            MediaKind::Comic if self.comicvine.is_none() => Some(
                "comics need a ComicVine API key (COMICVINE_API_KEY or [api] comicvine) for lookups; using the parsed name instead",
            ),
            _ => None,
        }
    }
}

/// `"2021-03-04"` → `2021`
pub(crate) fn year_of(date: Option<&str>) -> Option<u16> {
    date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

/// Drop HTML tags and collapse whitespace in a description.
pub(crate) fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
