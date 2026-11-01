//! Comic and manga filename parser.
//!
//! Handles `One Piece v01.cbz`, `One Piece - Chapter 1000.cbz`,
//! `Saga #001 (2012) (Digital) (Zone-Empire).cbz`, `Batman Annual 02 (2018).cbr`.

use std::path::Path;

use super::ParsedInfo;
use super::tokens::{
    BOUNDARY, BRACKETS, LEADING_GROUP, YEAR, YEAR_ONLY, clean_title, normalize_separators,
    parse_num, re,
};

re!(
    VOLUME,
    r"(?i)\b(?:vol(?:ume)?\.?|v)\s?(\d{1,4}(?:\.\d+)?)\b"
);
re!(
    CHAPTER,
    r"(?i)\b(?:ch(?:apter|ap)?\.?|c)\s?(\d{1,4}(?:\.\d+)?)\b"
);
re!(ISSUE, r"#\s?(\d{1,4}(?:\.\d+)?)");
re!(ANNUAL, r"(?i)\bannual\b");
re!(
    BARE_NUMBER,
    "(?:^|\\s)-?\\s?(\\d{1,4}(?:\\.\\d+)?)(?:\\s|$|\u{1})"
);
re!(OF_TOTAL, r"(?i)^of\s+\d+$");

pub fn parse(stem: &str) -> ParsedInfo {
    let mut info = ParsedInfo::default();
    let mut rest = stem.to_string();

    if let Some(c) = LEADING_GROUP.captures(&rest) {
        info.group = Some(c[1].trim().to_string());
        info.bracket_group = true;
        let end = c.get(0).unwrap().end();
        rest = rest[end..].to_string();
    }

    // Comics carry a lot of bracketed noise: (Digital) (Zone-Empire) (of 12).
    // Keep only the year; everything else becomes a title boundary.
    let mut out = String::with_capacity(rest.len());
    let mut last = 0;
    for c in BRACKETS.captures_iter(&rest) {
        let whole = c.get(0).unwrap();
        out.push_str(&rest[last..whole.start()]);
        last = whole.end();
        let content = c
            .get(1)
            .or(c.get(2))
            .or(c.get(3))
            .map(|m| m.as_str().trim())
            .unwrap_or("");
        if YEAR_ONLY.is_match(content) {
            info.year.get_or_insert(content.parse().unwrap_or(0));
        } else if OF_TOTAL.is_match(content) {
            // "(of 12)": mini-series length, not useful for naming.
        } else if c.get(1).is_some() && info.group.is_none() && !content.is_empty() {
            info.group = Some(content.to_string());
        }
        out.push(' ');
        out.push(BOUNDARY);
        out.push(' ');
    }
    out.push_str(&rest[last..]);

    let s = normalize_separators(&out);
    let mut title_end = s.find(BOUNDARY).unwrap_or(s.len());
    let mut mark = |pos: usize| title_end = title_end.min(pos);

    // Only look for markers after the first word so titles that start with a
    // number ("100 Bullets 001") keep it.
    let first_word_end = s.find(' ').unwrap_or(0);

    if let Some(c) = VOLUME.captures(&s) {
        mark(c.get(0).unwrap().start());
        info.volume = parse_num(&c[1]);
    }
    if let Some(c) = CHAPTER.captures(&s) {
        mark(c.get(0).unwrap().start());
        info.chapter = parse_num(&c[1]);
    }
    if let Some(c) = ISSUE.captures(&s) {
        mark(c.get(0).unwrap().start());
        info.issue = parse_num(&c[1]);
    }
    // "Annual" stays in the title: ComicVine lists "Batman Annual" as its own volume.
    info.annual = ANNUAL.is_match(&s);

    if let Some(m) = YEAR.find_iter(&s).filter(|m| m.start() > 0).last() {
        if info.year.is_none() {
            info.year = m.as_str().parse().ok();
        }
        // A year right after the title ends it ("Batman 2016 001").
        mark(m.start());
    }

    if info.issue.is_none() && info.chapter.is_none() {
        let number = BARE_NUMBER
            .captures_iter(&s)
            .filter(|c| c.get(1).unwrap().start() >= first_word_end)
            .find(|c| !YEAR_ONLY.is_match(&c[1]) && !inside_volume(&s, c.get(1).unwrap().start()));
        if let Some(c) = number {
            mark(c.get(0).unwrap().start());
            info.number = parse_num(&c[1]);
        }
    }

    info.title = clean_title(&s[..title_end]);
    info
}

/// Whether the number at `pos` is the one already consumed by `v01`/`Vol. 1`.
fn inside_volume(s: &str, pos: usize) -> bool {
    VOLUME
        .captures_iter(s)
        .any(|c| c.get(1).unwrap().start() == pos)
}

/// Fill a missing title (and year) from the parent folder:
/// `Saga (2012)/001.cbz`.
pub fn apply_folder_context(info: &mut ParsedInfo, path: &Path) {
    let Some(dir) = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return;
    };
    let dir_info = parse(&dir);
    let Some(dir_title) = dir_info.title else { return };

    let numeric_only = info
        .title
        .as_deref()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_digit() || c == '.'));

    if info.title.is_none() || numeric_only {
        if numeric_only && info.number.is_none() {
            info.number = info.title.as_deref().and_then(parse_num);
        }
        info.title = Some(dir_title);
        info.title_from_folder = true;
        if info.year.is_none() {
            info.year = dir_info.year;
        }
    } else if info.year.is_none()
        && let Some(t) = info.title.as_deref()
        && crate::matcher::normalize(t) == crate::matcher::normalize(&dir_title)
    {
        info.year = dir_info.year;
    }
}
