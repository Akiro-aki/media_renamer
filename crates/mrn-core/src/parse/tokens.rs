//! Shared lexical helpers for the video and book parsers.

use std::sync::LazyLock;

use regex::Regex;

/// Inserted where a removed bracket group used to be. It ends the title
/// (a title never continues past `(2021)` or `[1080p]`).
pub const BOUNDARY: char = '\u{1}';

macro_rules! re {
    ($name:ident, $pat:expr) => {
        pub static $name: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new($pat).unwrap());
    };
}
pub(crate) use re;

re!(LEADING_GROUP, r"^\s*\[([^\]]+)\]");
re!(BRACKETS, r"\[([^\]]*)\]|\(([^)]*)\)|\{([^}]*)\}");
re!(CRC32, r"^[0-9A-Fa-f]{8}$");
re!(YEAR_ONLY, r"^(19\d{2}|20\d{2})$");
re!(YEAR, r"\b(19\d{2}|20\d{2})\b");
re!(WHITESPACE, r"\s+");

/// What a technical token describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TechField {
    Resolution,
    Source,
    VideoCodec,
    AudioCodec,
    Other,
}

#[derive(Debug, Clone)]
pub struct TechHit {
    pub start: usize,
    pub end: usize,
    pub field: TechField,
    pub value: String,
    /// Whether this token reliably marks the end of a title. Ambiguous words
    /// such as "web" (Charlotte's Web) only fill fields.
    pub terminates: bool,
}

struct TechPattern {
    re: Regex,
    field: TechField,
    terminates: bool,
}

static TECH: LazyLock<Vec<TechPattern>> = LazyLock::new(|| {
    let p = |pat: &str, field, terminates| TechPattern {
        re: Regex::new(&format!(r"(?i)\b(?:{pat})\b")).unwrap(),
        field,
        terminates,
    };
    vec![
        p(r"\d{3,4}[pi]|4k|uhd|2160p", TechField::Resolution, true),
        p(r"\d{3,4}x(\d{3,4})", TechField::Resolution, true),
        p(
            r"blu-?ray|bdrip|brrip|bdremux|web-?dl|web-?rip|hdtv|pdtv|dvdrip|dvd[59]?|remux|hdrip|tvrip|bdmv",
            TechField::Source,
            true,
        ),
        p(r"web|bd", TechField::Source, false),
        p(
            r"[xh]\s?26[45]|hevc|avc|av1|xvid|divx|vp9|mpeg-?2",
            TechField::VideoCodec,
            true,
        ),
        p(
            r"aac(?:\s?\d(?:[\s.]\d)?)?|e?ac-?3|dts(?:-?hd)?(?:\s?ma)?|ddp?(?:\s?\d(?:[\s.]\d)?)?|flac|truehd|atmos|mp3",
            TechField::AudioCodec,
            true,
        ),
        p(r"opus", TechField::AudioCodec, false),
        p(
            r"proper|repack|internal|unrated|remastered|10-?bit|8-?bit|hdr(?:10)?(?:\+)?|dual[\s-]audio|multi-?subs?|batch|complete",
            TechField::Other,
            true,
        ),
    ]
});

/// Find every technical token in a normalized string.
pub fn find_tech(s: &str) -> Vec<TechHit> {
    let mut hits = Vec::new();
    for pat in TECH.iter() {
        for m in pat.re.captures_iter(s) {
            let whole = m.get(0).unwrap();
            let value = match pat.field {
                // 1920x1080 -> 1080p
                TechField::Resolution if m.get(1).is_some() => format!("{}p", &m[1]),
                TechField::Resolution => normalize_resolution(whole.as_str()),
                _ => whole.as_str().to_string(),
            };
            hits.push(TechHit {
                start: whole.start(),
                end: whole.end(),
                field: pat.field,
                value,
                terminates: pat.terminates,
            });
        }
    }
    hits.sort_by_key(|h| h.start);
    hits
}

fn normalize_resolution(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "4k" | "uhd" => "2160p".into(),
        _ => lower,
    }
}

/// Replace `_` and word-separating `.` with spaces and collapse whitespace.
/// Dots between digits survive (`5.1`, `12.5`, `2024.01.02`).
pub fn normalize_separators(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '_' => out.push(' '),
            '.' => {
                let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
                let next_digit = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
                if prev_digit && next_digit {
                    out.push('.');
                } else {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    WHITESPACE.replace_all(&out, " ").trim().to_string()
}

/// Trim separators and stray punctuation from both ends of a title and
/// collapse whitespace. Returns `None` for an empty result.
pub fn clean_title(s: &str) -> Option<String> {
    let s = s.replace(BOUNDARY, " ");
    let s = WHITESPACE.replace_all(&s, " ");
    let trimmed = s.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '-' | '_' | '.' | ',' | '~' | '+' | '|' | ':')
    });
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse a decimal number like `12`, `012`, `12.5`.
pub fn parse_num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Is this a season folder name (`Season 2`, `S02`, `Specials`)? Returns the
/// season number.
pub fn season_folder(name: &str) -> Option<u32> {
    static SEASON_DIR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:season|series|staffel|saison|temporada)[\s._-]*(\d{1,3})$|^s(\d{1,3})$")
            .unwrap()
    });
    let name = name.trim();
    if name.eq_ignore_ascii_case("specials") || name.eq_ignore_ascii_case("extras") {
        return Some(0);
    }
    let c = SEASON_DIR.captures(name)?;
    c.get(1).or(c.get(2))?.as_str().parse().ok()
}
