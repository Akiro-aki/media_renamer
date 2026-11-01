//! Built-in naming presets. Each kind maps to a list of templates; the first
//! one whose required fields are all present wins (e.g. a daily show has a
//! date but no SxxEyy, a one-shot comic has no issue number).

use crate::MediaKind;

pub const PRESETS: &[&str] = &["jellyfin", "plex", "kodi", "anime", "kavita", "komga"];

// Video
const JF_MOVIE: &str = r"{n}[ ({y})][ \[tmdbid-{tmdbid}\]]/{n}[ ({y})]";
const PLEX_MOVIE: &str = r"{n}[ ({y})][ \{tmdb-{tmdbid}\}]/{n}[ ({y})]";
const KODI_MOVIE: &str = r"{n}[ ({y})]/{n}[ ({y})]";

const TV_EPISODE: &str = r"{n}[ ({y})]/Season {s:02}/{n} - {s00e00}[ - {t}]";
const TV_DAILY: &str = r"{n}[ ({y})]/{n} - {d}[ - {t}]";
const KODI_EPISODE: &str = r"{n}/Season {s}/{n} {s00e00}[ - {t}]";
const KODI_DAILY: &str = r"{n}/{n} {d}[ - {t}]";

const ANIME_ABSOLUTE: &str = r"{n}[ ({y})]/{n} - {abs:02}[ - {t}]";
const ANIME_ABSOLUTE_FLAT: &str = r"{n}/{n} - {abs:02}[ - {t}]";

// Books
const COMIC_ISSUE: &str = r"{n}[ ({y})]/{n}[ ({y})] #{issue:03}";
const COMIC_ONESHOT: &str = r"{n}[ ({y})]/{n}[ ({y})]";
const MANGA_KAVITA: &str = r"{n}/{n}[ Vol.{vol:02}][ Ch.{ch:03}]";
const MANGA_KOMGA: &str = r"{n}/{n}[ v{vol:02}][ c{ch:03}]";

/// Templates for `preset` and `kind`, in fallback order.
pub fn templates(preset: &str, kind: MediaKind) -> Option<Vec<&'static str>> {
    use MediaKind::*;
    let video_tv = |ep, daily| vec![ep, daily];
    let t = match (preset, kind) {
        ("jellyfin", Movie) => vec![JF_MOVIE],
        ("plex", Movie) => vec![PLEX_MOVIE],
        ("kodi", Movie) => vec![KODI_MOVIE],
        ("anime", Movie) => vec![JF_MOVIE],

        ("jellyfin" | "plex" | "anime", Tv) => video_tv(TV_EPISODE, TV_DAILY),
        ("kodi", Tv) => video_tv(KODI_EPISODE, KODI_DAILY),

        // Season/episode when known (TMDB mapping), absolute otherwise.
        ("jellyfin" | "plex", Anime) => vec![TV_EPISODE, ANIME_ABSOLUTE],
        ("kodi", Anime) => vec![KODI_EPISODE, ANIME_ABSOLUTE],
        ("anime", Anime) => vec![ANIME_ABSOLUTE_FLAT, TV_EPISODE],

        (_, Comic) if PRESETS.contains(&preset) => vec![COMIC_ISSUE, COMIC_ONESHOT],
        ("komga", Manga) => vec![MANGA_KOMGA],
        (_, Manga) if PRESETS.contains(&preset) => vec![MANGA_KAVITA],

        // Book presets used for video: fall back to Jellyfin layouts.
        ("kavita" | "komga", Movie) => vec![JF_MOVIE],
        ("kavita" | "komga", Tv) => video_tv(TV_EPISODE, TV_DAILY),
        ("kavita" | "komga", Anime) => vec![TV_EPISODE, ANIME_ABSOLUTE],
        _ => return None,
    };
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Template;

    #[test]
    fn every_preset_parses() {
        for p in PRESETS {
            for k in MediaKind::ALL {
                for t in templates(p, k).unwrap() {
                    Template::parse(t).unwrap_or_else(|e| panic!("{p}/{k}: {e}"));
                }
            }
        }
    }
}
