//! mrn-core: parse, match, format and rename media files.
//!
//! The pipeline is `scan` → `parse` → `detect` → `matcher` (via `providers`)
//! → `format` → `ops` (recorded in `history`), with optional `extras`.
//! The CLI is a thin layer over [`pipeline`]; a GUI can reuse the same API.

pub mod cache;
pub mod config;
pub mod detect;
pub mod extras;
pub mod format;
pub mod history;
pub mod matcher;
pub mod ops;
pub mod parse;
pub mod pipeline;
pub mod preview;
pub mod providers;
pub mod scan;
pub mod trash;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The kind of media a file holds. Decides which providers and naming
/// templates are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Movie,
    Tv,
    Anime,
    Comic,
    Manga,
}

impl MediaKind {
    pub const ALL: [MediaKind; 5] = [
        MediaKind::Movie,
        MediaKind::Tv,
        MediaKind::Anime,
        MediaKind::Comic,
        MediaKind::Manga,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Movie => "movie",
            MediaKind::Tv => "tv",
            MediaKind::Anime => "anime",
            MediaKind::Comic => "comic",
            MediaKind::Manga => "manga",
        }
    }

    pub fn is_video(self) -> bool {
        matches!(self, MediaKind::Movie | MediaKind::Tv | MediaKind::Anime)
    }

    pub fn is_book(self) -> bool {
        !self.is_video()
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MediaKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "movie" | "movies" | "film" => Ok(MediaKind::Movie),
            "tv" | "series" | "show" | "episode" => Ok(MediaKind::Tv),
            "anime" => Ok(MediaKind::Anime),
            "comic" | "comics" => Ok(MediaKind::Comic),
            "manga" => Ok(MediaKind::Manga),
            other => Err(format!(
                "unknown media type '{other}' (expected movie, tv, anime, comic or manga)"
            )),
        }
    }
}
