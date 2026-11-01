//! Video filename parser (movies, TV, anime).

use std::path::Path;

use super::tokens::{
    BOUNDARY, BRACKETS, CRC32, LEADING_GROUP, TechField, YEAR, YEAR_ONLY, clean_title, find_tech,
    normalize_separators, re, season_folder,
};
use super::{AirDate, ParsedInfo};

re!(
    SXXEYY,
    r"(?i)\bS(\d{1,3})[\s.]?E(\d{1,4})((?:[\s-]*E\d{1,4})*)(?:-(\d{1,4}))?(?:v(\d))?\b"
);
re!(NXNN, r"(?i)\b(\d{1,2})x(\d{2,3})\b");
re!(
    DATE,
    r"\b((?:19|20)\d{2})[.\s-](\d{2})[.\s-](\d{2})\b"
);
re!(SEASON_ONLY, r"(?i)\b(?:season\s?(\d{1,3})|S(\d{1,2}))\b");
re!(EP_WORD, r"(?i)\b(?:episode|ep)\.?\s?(\d{1,4})(?:v(\d))?\b");
re!(E_ONLY, r"(?i)\bE(\d{2,4})(?:v(\d))?\b");
re!(
    DASH_NUMBER,
    "(?:^|\\s)-\\s?(\\d{1,4})(?:v(\\d))?(?:\\s|$|\u{1})"
);
re!(BRACKET_EP, r"^(\d{1,4})(?:v(\d))?$");
re!(EXTRA_E, r"(?i)E(\d{1,4})");
re!(TRAILING_GROUP, r"[^\s-]-([A-Za-z0-9]+)\s*$");
re!(TRAILING_NUMBER, r"\s(\d{1,4})(?:v(\d))?$");
re!(DIGITS_ONLY, r"^(\d{1,4})(?:v\d)?$");
re!(LEADING_EP, r"(?i)^(?:e|ep\s?)?(\d{1,3})(?:v\d)?(?:\s*-\s*|\s+|$)");

pub fn parse(stem: &str) -> ParsedInfo {
    let mut info = ParsedInfo::default();
    let mut rest = stem.to_string();

    // 1. Leading fansub group: "[SubsPlease] Title - 01"
    if let Some(c) = LEADING_GROUP.captures(&rest) {
        let group = c[1].trim().to_string();
        // A leading bracket that is really a tag (e.g. "[1080p] Title") is not a group.
        if !group.is_empty() && find_tech(&group).is_empty() {
            info.group = Some(group);
            info.bracket_group = true;
            let end = c.get(0).unwrap().end();
            rest = rest[end..].to_string();
        }
    }

    // 2. Bracket groups: classify and remove, leaving a title boundary behind.
    let mut bracket_ep: Option<(u32, Option<u32>)> = None;
    let mut unclassified_square: Option<String> = None;
    let mut out = String::with_capacity(rest.len());
    let mut last = 0;
    for c in BRACKETS.captures_iter(&rest) {
        let whole = c.get(0).unwrap();
        out.push_str(&rest[last..whole.start()]);
        last = whole.end();

        let (content, is_paren) = match (c.get(1), c.get(2), c.get(3)) {
            (Some(m), _, _) => (m.as_str().trim(), false),
            (_, Some(m), _) => (m.as_str().trim(), true),
            (_, _, Some(m)) => (m.as_str().trim(), false),
            _ => ("", false),
        };

        if CRC32.is_match(content) {
            info.crc32 = Some(content.to_ascii_uppercase());
        } else if YEAR_ONLY.is_match(content) {
            info.year = content.parse().ok();
        } else if let (false, Some(ep)) = (is_paren, BRACKET_EP.captures(content)) {
            bracket_ep = Some((
                ep[1].parse().unwrap_or(0),
                ep.get(2).and_then(|v| v.as_str().parse().ok()),
            ));
        } else {
            let normalized = normalize_separators(content);
            let hits = find_tech(&normalized);
            if !hits.is_empty() {
                apply_tech(&mut info, &hits);
            } else if is_paren {
                // "(US)", "(Director's Cut)": part of the title text.
                out.push_str(whole.as_str());
                continue;
            } else if content.chars().any(char::is_alphabetic) {
                // Last wins: "[1080p] [5.1] [YTS.MX]" → YTS.MX
                unclassified_square = Some(content.to_string());
            }
        }
        out.push(' ');
        out.push(BOUNDARY);
        out.push(' ');
    }
    out.push_str(&rest[last..]);

    let s = normalize_separators(&out);
    let mut title_end = s.len();
    let mut mark = |pos: usize| {
        if pos < title_end {
            title_end = pos;
        }
    };

    if let Some(pos) = s.find(BOUNDARY) {
        mark(pos);
    }

    // 3. Episode markers, strongest first.
    if let Some(c) = SXXEYY.captures(&s) {
        mark(c.get(0).unwrap().start());
        info.season = c[1].parse().ok();
        let first: u32 = c[2].parse().unwrap_or(0);
        let mut eps = vec![first];
        let extra = c.get(3).map(|m| m.as_str()).unwrap_or("");
        for e in EXTRA_E.captures_iter(extra) {
            if let Ok(n) = e[1].parse() {
                eps.push(n);
            }
        }
        if let Some(range_end) = c.get(4).and_then(|m| m.as_str().parse::<u32>().ok()) {
            eps.push(range_end);
        }
        // "E01-E03" / "E01-03" is a range; "E01E02" is a list.
        let is_range = eps.len() == 2 && extra.contains('-') || c.get(4).is_some();
        if is_range && eps.len() == 2 && eps[1] > eps[0] && eps[1] - eps[0] <= 30 {
            eps = (eps[0]..=eps[1]).collect();
        }
        eps.dedup();
        info.episodes = eps;
        info.version = c.get(5).and_then(|m| m.as_str().parse().ok());
    } else if let Some(c) = NXNN.captures(&s) {
        mark(c.get(0).unwrap().start());
        info.season = c[1].parse().ok();
        info.episodes = vec![c[2].parse().unwrap_or(0)];
    }

    let mut date_span = None;
    if info.episodes.is_empty()
        && let Some(c) = DATE.captures(&s)
    {
        let (y, m, d) = (
            c[1].parse::<u16>().unwrap_or(0),
            c[2].parse::<u8>().unwrap_or(0),
            c[3].parse::<u8>().unwrap_or(0),
        );
        if (1..=12).contains(&m) && (1..=31).contains(&d) {
            let whole = c.get(0).unwrap();
            mark(whole.start());
            date_span = Some(whole.range());
            info.date = Some(AirDate { year: y, month: m, day: d });
        }
    }

    if info.season.is_none()
        && let Some(c) = SEASON_ONLY.captures(&s)
    {
        mark(c.get(0).unwrap().start());
        info.season = c.get(1).or(c.get(2)).and_then(|m| m.as_str().parse().ok());
    }

    if info.episodes.is_empty() && info.date.is_none() {
        let found = DASH_NUMBER
            .captures(&s)
            .filter(|c| !YEAR_ONLY.is_match(&c[1]))
            .or_else(|| EP_WORD.captures(&s))
            .or_else(|| E_ONLY.captures(&s));
        if let Some(c) = found {
            mark(c.get(0).unwrap().start());
            info.absolute = vec![c[1].parse().unwrap_or(0)];
            info.version = c.get(2).and_then(|m| m.as_str().parse().ok());
        } else if let Some((ep, v)) = bracket_ep {
            info.absolute = vec![ep];
            info.version = v;
        }
    }

    // 4. Year: the last plausible year that is not the very first word
    //    ("1917.2019.mkv" → title "1917", year 2019; "2012.mkv" → no year).
    if info.year.is_none() {
        let year = YEAR
            .find_iter(&s)
            .filter(|m| m.start() > 0)
            .filter(|m| date_span.as_ref().is_none_or(|d| !d.contains(&m.start())))
            .last();
        if let Some(m) = year {
            mark(m.start());
            info.year = m.as_str().parse().ok();
        }
    }

    // 5. Technical tokens.
    let hits = find_tech(&s);
    if let Some(h) = hits.iter().find(|h| h.terminates) {
        mark(h.start);
    }
    apply_tech(&mut info, &hits);

    // 6. Scene group: "...x264-GROUP" (but not the "DL" of "WEB-DL").
    if info.group.is_none()
        && title_end < s.len()
        && let Some(c) = TRAILING_GROUP.captures(&s)
    {
        let g = c.get(1).unwrap();
        let dash = g.start() - 1;
        let tech_spans_dash = hits.iter().any(|h| h.start < dash && h.end > dash);
        if g.start() > title_end && !tech_spans_dash && find_tech(g.as_str()).is_empty() {
            info.group = Some(g.as_str().to_string());
        }
    }
    if info.group.is_none() {
        info.group = unclassified_square;
    }

    let mut title = clean_title(&s[..title_end]);

    // 7. Fansub release with the episode glued to the title: "[Grp] Show 03 [1080p]"
    if info.bracket_group
        && info.absolute.is_empty()
        && info.episodes.is_empty()
        && let Some(t) = title.clone()
        && let Some(c) = TRAILING_NUMBER.captures(&t)
    {
        let head = clean_title(&t[..c.get(0).unwrap().start()]);
        if head.is_some() && !YEAR_ONLY.is_match(&c[1]) {
            info.absolute = vec![c[1].parse().unwrap_or(0)];
            info.version = c.get(2).and_then(|m| m.as_str().parse().ok());
            title = head;
        }
    }

    // "Show S2 - 03": a season plus a relative episode number.
    if info.season.is_some() && info.episodes.is_empty() && !info.absolute.is_empty() {
        info.episodes = std::mem::take(&mut info.absolute);
    }

    info.title = title;
    info
}

fn apply_tech(info: &mut ParsedInfo, hits: &[super::tokens::TechHit]) {
    for h in hits {
        let slot = match h.field {
            TechField::Resolution => &mut info.resolution,
            TechField::Source => &mut info.source,
            TechField::VideoCodec => &mut info.video_codec,
            TechField::AudioCodec => &mut info.audio_codec,
            TechField::Other => continue,
        };
        if slot.is_none() {
            *slot = Some(h.value.clone());
        }
    }
}

/// Fill gaps from parent folders: `Show (2015)/Season 2/03 - Title.mkv`.
pub fn apply_folder_context(info: &mut ParsedInfo, path: &Path) {
    let mut season_from_dir = None;
    let mut show_dir = None;
    for dir in path.ancestors().skip(1) {
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy()) else {
            break;
        };
        if let Some(season) = season_folder(&name) {
            season_from_dir.get_or_insert(season);
            continue;
        }
        show_dir = Some(parse(&name));
        break;
    }

    // A file named only by its number ("03.mkv"), or, inside a season folder,
    // starting with it ("03 - Pilot.mkv").
    let numeric_title = info.title.as_deref().and_then(|t| {
        let re = if season_from_dir.is_some() && !info.has_episode_marker() {
            &*LEADING_EP
        } else {
            &*DIGITS_ONLY
        };
        re.captures(t).and_then(|c| c[1].parse::<u32>().ok())
    });
    let generic = info.title.as_deref().is_some_and(|t| {
        matches!(
            t.to_ascii_lowercase().as_str(),
            "movie" | "film" | "video" | "main" | "feature" | "episode"
        )
    });
    let needs_title = info.title.is_none() || numeric_title.is_some() || generic;

    if info.season.is_none()
        && let Some(season) = season_from_dir
    {
        // Only trust a season folder for files that look episodic.
        if info.has_episode_marker() || !info.absolute.is_empty() || numeric_title.is_some() {
            info.season = Some(season);
            if info.episodes.is_empty() && !info.absolute.is_empty() {
                info.episodes = std::mem::take(&mut info.absolute);
            }
        }
    }

    let Some(dir) = show_dir else { return };
    let Some(dir_title) = dir.title.clone() else { return };

    if needs_title {
        if let Some(n) = numeric_title {
            if info.season.is_some() {
                info.episodes = vec![n];
            } else {
                info.absolute = vec![n];
            }
        }
        info.title = Some(dir_title);
        info.title_from_folder = true;
        if info.year.is_none() {
            info.year = dir.year;
        }
    } else if info.year.is_none()
        && let Some(t) = info.title.as_deref()
        && crate::matcher::normalize(t) == crate::matcher::normalize(&dir_title)
    {
        info.year = dir.year;
    }
}
