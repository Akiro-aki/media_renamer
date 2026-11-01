//! Find media files under the given inputs and attach their sidecar files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::parse::{extension_of, is_book_ext, is_video_ext};

/// Files that travel with a video: subtitles, nfo, thumbnails.
pub const SIDECAR_EXTENSIONS: &[&str] = &[
    "srt", "ass", "ssa", "sub", "idx", "vtt", "sup", "smi", "nfo", "jpg", "jpeg", "png", "txt",
];

/// Videos smaller than this with "sample" in the name are skipped.
const SAMPLE_MAX_BYTES: u64 = 300 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MediaFile {
    pub path: PathBuf,
    /// Sidecar path and the suffix after the video's stem (`.en.srt`).
    pub sidecars: Vec<(PathBuf, String)>,
    /// The input argument this file was found under.
    pub root: PathBuf,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<MediaFile>,
    /// Explicitly given files that are not media.
    pub ignored: Vec<PathBuf>,
}

pub fn scan(inputs: &[PathBuf], recursive: bool) -> Result<ScanResult> {
    let mut found: BTreeMap<PathBuf, PathBuf> = BTreeMap::new(); // path -> root
    let mut ignored = Vec::new();

    for input in inputs {
        let meta = fs::metadata(input).with_context(|| format!("cannot read {}", input.display()))?;
        if meta.is_file() {
            if is_media(input) {
                found.entry(input.clone()).or_insert_with(|| {
                    input.parent().map(Path::to_path_buf).unwrap_or_default()
                });
            } else {
                ignored.push(input.clone());
            }
            continue;
        }
        let walker = WalkDir::new(input)
            .max_depth(if recursive { usize::MAX } else { 1 })
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()));
        for entry in walker {
            let entry = entry?;
            if entry.file_type().is_file() && is_media(entry.path()) && !is_sample(entry.path()) {
                found
                    .entry(entry.path().to_path_buf())
                    .or_insert_with(|| input.clone());
            }
        }
    }

    let sidecars = find_sidecars(found.keys())?;
    let files = found
        .into_iter()
        .map(|(path, root)| MediaFile {
            sidecars: sidecars.get(&path).cloned().unwrap_or_default(),
            path,
            root,
        })
        .collect();
    Ok(ScanResult { files, ignored })
}

fn is_media(p: &Path) -> bool {
    let ext = extension_of(p);
    is_video_ext(&ext) || is_book_ext(&ext)
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn is_sample(p: &Path) -> bool {
    if !is_video_ext(&extension_of(p)) {
        return false;
    }
    let lower = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let is_sample_name = lower == "sample"
        || lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == "sample");
    is_sample_name && fs::metadata(p).map(|m| m.len() < SAMPLE_MAX_BYTES).unwrap_or(false)
}

/// For every video, find sibling files named `<stem>.<anything>.<sidecar ext>`.
/// When several videos share a prefix the longest stem wins.
fn find_sidecars<'a>(
    media: impl Iterator<Item = &'a PathBuf>,
) -> Result<BTreeMap<PathBuf, Vec<(PathBuf, String)>>> {
    let mut by_dir: BTreeMap<PathBuf, Vec<(String, PathBuf)>> = BTreeMap::new();
    for p in media {
        if !is_video_ext(&extension_of(p)) {
            continue;
        }
        let (Some(dir), Some(stem)) = (p.parent(), p.file_stem()) else {
            continue;
        };
        by_dir
            .entry(dir.to_path_buf())
            .or_default()
            .push((stem.to_string_lossy().into_owned(), p.clone()));
    }

    let mut out: BTreeMap<PathBuf, Vec<(PathBuf, String)>> = BTreeMap::new();
    for (dir, mut videos) in by_dir {
        videos.sort_by_key(|(stem, _)| std::cmp::Reverse(stem.len()));
        let lookup_dir = if dir.as_os_str().is_empty() { Path::new(".") } else { &dir };
        let Ok(entries) = fs::read_dir(lookup_dir) else { continue };
        let names: BTreeSet<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        for name in names {
            let ext = extension_of(Path::new(&name));
            if !SIDECAR_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            let owner = videos.iter().find(|(stem, _)| {
                name.strip_prefix(stem.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
            });
            if let Some((stem, video)) = owner {
                let suffix = name[stem.len()..].to_string();
                out.entry(video.clone())
                    .or_default()
                    .push((dir.join(&name), suffix));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_media_and_sidecars() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for f in [
            "Show.S01E01.mkv",
            "Show.S01E01.en.srt",
            "Show.S01E01.nfo",
            "Show.S01E01.Extended.mkv",
            "Show.S01E01.Extended.srt",
            "notes.txt",
            "sample.mkv",
            ".hidden.mkv",
            "sub/Comic #001.cbz",
        ] {
            let path = p.join(f);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        let r = scan(&[p.to_path_buf()], true).unwrap();
        let names: Vec<String> = r
            .files
            .iter()
            .map(|f| f.path.strip_prefix(p).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Show.S01E01.Extended.mkv", "Show.S01E01.mkv", "sub/Comic #001.cbz"]);

        let ep = r.files.iter().find(|f| f.path.ends_with("Show.S01E01.mkv")).unwrap();
        let suffixes: Vec<&str> = ep.sidecars.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(suffixes, [".en.srt", ".nfo"]);
        let ext = r.files.iter().find(|f| f.path.ends_with("Show.S01E01.Extended.mkv")).unwrap();
        assert_eq!(ext.sidecars.len(), 1);

        let flat = scan(&[p.to_path_buf()], false).unwrap();
        assert_eq!(flat.files.len(), 2);
    }
}
