//! Optional extras written next to renamed files: Kodi/Jellyfin `.nfo`,
//! artwork, and ComicInfo.xml and covers inside `.cbz` archives. All off by
//! default.

pub mod comicinfo;
pub mod cover;
pub mod nfo;

use std::path::{Path, PathBuf};

use crate::MediaKind;
use crate::config::Extras;
use crate::parse::season_folder;
use crate::providers::{Candidate, Episode, SeriesInfo};

/// A file to create, either from text or by downloading a URL.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtraContent {
    Text(String),
    Download(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtraFile {
    pub path: PathBuf,
    pub content: ExtraContent,
}

/// Everything extras need to know about one renamed file.
pub struct ExtraContext<'a> {
    pub kind: MediaKind,
    pub target: &'a Path,
    /// True when the target sits in a library folder layout (an output root
    /// was used). Series-level files (tvshow.nfo, poster.jpg) are only
    /// written then, never into a downloads folder.
    pub organized: bool,
    pub candidate: &'a Candidate,
    pub series: Option<&'a SeriesInfo>,
    pub episode: Option<&'a Episode>,
    pub season: Option<u32>,
    pub episode_numbers: &'a [u32],
    /// This book's own cover (its volume's, or the comic issue's).
    pub book_cover: Option<&'a str>,
    /// Save `book_cover` beside the book as `<stem>.<ext>` (Komga reads
    /// these; Kavita would take them as an extra chapter).
    pub book_sidecar: bool,
}

/// The folder a series lives in: the target's folder, or its parent when
/// that folder is a season folder.
pub fn series_dir(target: &Path) -> Option<PathBuf> {
    let parent = target.parent()?;
    let name = parent.file_name()?.to_string_lossy();
    if season_folder(&name).is_some() {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

/// Files to create for one renamed file. Existing files are filtered out by
/// the caller, so user-made art and nfo files are never overwritten.
pub fn plan(ctx: &ExtraContext, extras: &Extras) -> Vec<ExtraFile> {
    let mut out = Vec::new();
    let c = ctx.candidate;
    let stem = ctx
        .target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let beside = |suffix: &str| ctx.target.with_file_name(format!("{stem}{suffix}"));
    let text = |path: PathBuf, s: String| ExtraFile {
        path,
        content: ExtraContent::Text(s),
    };
    let download = |path: PathBuf, url: &str| ExtraFile {
        path,
        content: ExtraContent::Download(url.to_string()),
    };

    match ctx.kind {
        MediaKind::Movie => {
            if extras.nfo {
                out.push(text(beside(".nfo"), nfo::movie(c)));
            }
            if extras.artwork {
                if let Some(url) = &c.poster {
                    out.push(download(beside(&format!("-poster.{}", image_ext(url))), url));
                }
                if let Some(url) = &c.backdrop {
                    out.push(download(beside(&format!("-fanart.{}", image_ext(url))), url));
                }
            }
        }
        MediaKind::Tv | MediaKind::Anime => {
            let series_dir = series_dir(ctx.target).filter(|_| ctx.organized);
            if extras.nfo {
                out.push(text(
                    beside(".nfo"),
                    nfo::episode(c, ctx.episode, ctx.season, ctx.episode_numbers),
                ));
                if let Some(dir) = &series_dir {
                    out.push(text(dir.join("tvshow.nfo"), nfo::tvshow(c)));
                }
            }
            if extras.artwork
                && let Some(dir) = &series_dir
            {
                if let Some(url) = &c.poster {
                    out.push(download(dir.join(format!("poster.{}", image_ext(url))), url));
                }
                if let Some(url) = &c.backdrop {
                    out.push(download(dir.join(format!("fanart.{}", image_ext(url))), url));
                }
                if let (Some(s), Some(series)) = (ctx.season, ctx.series)
                    && let Some(url) = series.season_posters.get(&s)
                {
                    let name = if s == 0 {
                        format!("season-specials-poster.{}", image_ext(url))
                    } else {
                        format!("season{s:02}-poster.{}", image_ext(url))
                    };
                    out.push(download(dir.join(name), url));
                }
            }
        }
        MediaKind::Comic | MediaKind::Manga => {
            if extras.artwork
                && ctx.organized
                && let (Some(dir), Some(url)) = (ctx.target.parent(), &c.poster)
            {
                out.push(download(dir.join(format!("cover.{}", image_ext(url))), url));
            }
            if extras.artwork
                && ctx.book_sidecar
                && let Some(url) = ctx.book_cover
            {
                out.push(download(beside(&format!(".{}", image_ext(url))), url));
            }
        }
    }
    out
}

/// Extension for a downloaded image, from its URL.
pub(crate) fn image_ext(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".png") {
        "png"
    } else if path.ends_with(".webp") {
        "webp"
    } else {
        "jpg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Source;

    fn show() -> Candidate {
        Candidate {
            source: Some(Source::Tmdb),
            id: "1".into(),
            title: "Show".into(),
            year: Some(2020),
            poster: Some("https://img/p.jpg".into()),
            backdrop: Some("https://img/b.png?x=1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn tv_extras_only_in_library_layout() {
        let c = show();
        let mut series = SeriesInfo::default();
        series.season_posters.insert(1, "https://img/s1.jpg".into());
        let target = Path::new("/lib/Show (2020)/Season 01/Show - S01E02.mkv");
        let all = Extras {
            nfo: true,
            artwork: true,
            ..Default::default()
        };
        let ctx = ExtraContext {
            kind: MediaKind::Tv,
            target,
            organized: true,
            candidate: &c,
            series: Some(&series),
            episode: None,
            season: Some(1),
            episode_numbers: &[2],
            book_cover: None,
            book_sidecar: false,
        };
        let paths: Vec<_> = plan(&ctx, &all).into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            [
                "/lib/Show (2020)/Season 01/Show - S01E02.nfo",
                "/lib/Show (2020)/tvshow.nfo",
                "/lib/Show (2020)/poster.jpg",
                "/lib/Show (2020)/fanart.png",
                "/lib/Show (2020)/season01-poster.jpg",
            ]
            .map(PathBuf::from)
        );

        let in_place = ExtraContext {
            organized: false,
            ..ctx
        };
        let paths: Vec<_> = plan(&in_place, &all).into_iter().map(|e| e.path).collect();
        assert_eq!(paths, [PathBuf::from("/lib/Show (2020)/Season 01/Show - S01E02.nfo")]);

        assert!(plan(&in_place, &Extras::default()).is_empty());
    }

    #[test]
    fn movie_extras_sit_beside_the_file() {
        let c = show();
        let ctx = ExtraContext {
            kind: MediaKind::Movie,
            target: Path::new("/dl/Movie (2020).mkv"),
            organized: false,
            candidate: &c,
            series: None,
            episode: None,
            season: None,
            episode_numbers: &[],
            book_cover: None,
            book_sidecar: false,
        };
        let extras = Extras {
            artwork: true,
            ..Default::default()
        };
        let paths: Vec<_> = plan(&ctx, &extras).into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            ["/dl/Movie (2020)-poster.jpg", "/dl/Movie (2020)-fanart.png"].map(PathBuf::from)
        );
    }

    #[test]
    fn book_cover_sidecar_only_when_asked() {
        let c = Candidate {
            poster: Some("https://img/series.jpg".into()),
            ..Default::default()
        };
        let ctx = ExtraContext {
            kind: MediaKind::Manga,
            target: Path::new("/lib/Frieren/Frieren v01.cbz"),
            organized: true,
            candidate: &c,
            series: None,
            episode: None,
            season: None,
            episode_numbers: &[],
            book_cover: Some("https://uploads/covers/x/v1.png"),
            book_sidecar: true,
        };
        let artwork = Extras {
            artwork: true,
            ..Default::default()
        };
        let paths: Vec<_> = plan(&ctx, &artwork).into_iter().map(|e| e.path).collect();
        assert_eq!(paths, ["/lib/Frieren/cover.jpg", "/lib/Frieren/Frieren v01.png"].map(PathBuf::from));
        let kavita = ExtraContext {
            book_sidecar: false,
            ..ctx
        };
        assert_eq!(plan(&kavita, &artwork).len(), 1, "no sidecar unless asked");
    }
}
