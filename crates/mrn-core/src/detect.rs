//! Decide what kind of media a parsed file is.

use crate::MediaKind;
use crate::parse::{ParsedInfo, is_book_ext};

/// Guess the media kind from the parsed fields. `--type` overrides this.
pub fn detect(info: &ParsedInfo) -> MediaKind {
    if is_book_ext(&info.extension) {
        return detect_book(info);
    }

    let fansub = info.bracket_group || info.crc32.is_some();
    if fansub && (!info.absolute.is_empty() || !info.episodes.is_empty()) {
        return MediaKind::Anime;
    }
    if !info.episodes.is_empty() || info.date.is_some() || info.season.is_some() {
        return MediaKind::Tv;
    }
    if !info.absolute.is_empty() {
        // Absolute numbering without seasons is an anime convention.
        return MediaKind::Anime;
    }
    MediaKind::Movie
}

fn detect_book(info: &ParsedInfo) -> MediaKind {
    if info.issue.is_some() || info.annual {
        return MediaKind::Comic;
    }
    if info.chapter.is_some() {
        return MediaKind::Manga;
    }
    // "Batman v2 001 (2016)": a western comic run, not a manga volume.
    if info.volume.is_some() {
        if info.number.is_some() && info.year.is_some() {
            return MediaKind::Comic;
        }
        return MediaKind::Manga;
    }
    if info.number.is_some() {
        return if info.year.is_some() {
            MediaKind::Comic
        } else {
            MediaKind::Manga
        };
    }
    if info.year.is_some() {
        MediaKind::Comic
    } else {
        MediaKind::Manga
    }
}

/// Book numbers are stored neutrally by the parser. Once the kind is known,
/// move a bare number into the field the templates use.
pub fn resolve_book_numbers(info: &mut ParsedInfo, kind: MediaKind) {
    let Some(n) = info.number else { return };
    match kind {
        MediaKind::Manga if info.chapter.is_none() => info.chapter = Some(n),
        MediaKind::Comic if info.issue.is_none() => info.issue = Some(n),
        _ => {}
    }
}
