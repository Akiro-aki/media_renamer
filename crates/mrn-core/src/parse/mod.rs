//! Filename parsing: turn `The.Expanse.S02E05.1080p.WEB.mkv` or
//! `[SubsPlease] Frieren - 12 (1080p) [ABCD1234].mkv` into structured fields.
//!
//! Parsing is purely lexical. Deciding *what* the file is happens in
//! [`crate::detect`], and looking it up happens in [`crate::providers`].

mod book;
mod tokens;
mod video;

use std::path::Path;

use serde::Serialize;

pub use tokens::{clean_title, normalize_separators, season_folder};

/// A calendar date taken from a filename (daily shows, issue dates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AirDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl std::fmt::Display for AirDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Everything we could read out of a file name (and its parent folders).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParsedInfo {
    /// Series or movie title as written in the filename.
    pub title: Option<String>,
    pub year: Option<u16>,
    pub season: Option<u32>,
    /// Episode numbers within `season` (several for multi-episode files).
    pub episodes: Vec<u32>,
    /// Absolute episode numbers (anime style `Show - 12`).
    pub absolute: Vec<u32>,
    pub date: Option<AirDate>,
    /// Release version (`12v2` → 2).
    pub version: Option<u32>,

    pub volume: Option<f64>,
    pub chapter: Option<f64>,
    /// Comic issue number (`#012`).
    pub issue: Option<f64>,
    /// A bare number after a book title (`One Piece 1000.cbz`). Detection
    /// decides whether it is a chapter (manga) or an issue (comic).
    pub number: Option<f64>,
    pub annual: bool,

    pub resolution: Option<String>,
    pub source: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// Release/scanlation group.
    pub group: Option<String>,
    /// True when the group came from a leading `[Group]` tag (fansub style).
    pub bracket_group: bool,
    pub crc32: Option<String>,
    /// Lower-case extension without the dot.
    pub extension: String,
    /// True when the title came from a parent folder rather than the file.
    pub title_from_folder: bool,
}

impl ParsedInfo {
    pub fn has_episode_marker(&self) -> bool {
        !self.episodes.is_empty() || self.date.is_some()
    }
}

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "mpg", "mpeg", "ts", "m2ts", "webm", "flv", "ogm",
    "divx", "rmvb", "3gp",
];

pub const BOOK_EXTENSIONS: &[&str] = &["cbz", "cbr", "cb7", "cbt", "pdf", "epub", "zip", "rar"];

pub fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext)
}

pub fn is_book_ext(ext: &str) -> bool {
    BOOK_EXTENSIONS.contains(&ext)
}

pub fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Parse a media file path. Uses the file name first and parent folder names
/// as fallback (e.g. `Show/Season 2/03.mkv`).
pub fn parse_path(path: &Path) -> ParsedInfo {
    let ext = extension_of(path);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut info = if is_book_ext(&ext) {
        book::parse(&stem)
    } else {
        video::parse(&stem)
    };
    info.extension = ext;

    if !is_book_ext(&info.extension) {
        video::apply_folder_context(&mut info, path);
    } else {
        book::apply_folder_context(&mut info, path);
    }
    info
}

/// Parse just a name (no folder context). Mostly useful for tests and `mrn info`.
pub fn parse_name(file_name: &str) -> ParsedInfo {
    parse_path(Path::new(file_name))
}
