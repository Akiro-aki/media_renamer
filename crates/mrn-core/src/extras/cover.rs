//! Volume and chapter covers embedded into `.cbz` archives for Kavita, which
//! only takes a book's cover from inside the archive.
//!
//! Kavita picks the first image (natural order) whose name matches its
//! `CoverImageRegex`, else the first image. An archive that already has such
//! a cover is left alone. Otherwise `!cover.<ext>` is added: it matches the
//! regex and `!` sorts before digits and letters, so other readers show it
//! first too. It counts as an extra page.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use zip::ZipArchive;

use super::comicinfo::{rewrite, supported};

/// The name an embedded cover gets, without its extension.
const COVER_STEM: &str = "!cover";

/// Kavita's image extensions (`Parser.ImageFileExtensions`).
const IMAGE_EXTS: &[&str] = &["png", "jpeg", "jpg", "webp", "gif", "avif"];

/// Kavita's `Parser.IsImage`: an image extension, not a dot file.
fn is_image(name: &str) -> bool {
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| IMAGE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Kavita's `Parser.IsCoverImage`. A port of `CoverImageRegex`:
///
/// ```text
/// (?<!back[\s_-])(?<!\(back )(?<!back)(?:^|[^a-zA-Z0-9])(!?cover|folder)(?![a-zA-Z0-9]|s\b)
/// ```
///
/// case-insensitive, matched anywhere in the entry's path. The `regex` crate
/// has no look-behind, so this tries each match start by hand. (The `s\b`
/// branch adds nothing: `s` is already alphanumeric.)
pub fn is_cover_image(name: &str) -> bool {
    if !is_image(name) {
        return false;
    }
    let s = name.to_ascii_lowercase();
    let b = s.as_bytes();
    let word_at = |w: usize| {
        let rest = &b[w.min(b.len())..];
        [&b"!cover"[..], b"cover", b"folder"].iter().any(|word| {
            rest.starts_with(word) && !rest.get(word.len()).is_some_and(|c| c.is_ascii_alphanumeric())
        })
    };
    (0..=b.len()).any(|m| {
        // The look-behinds, at the start of the match.
        let before = &b[..m];
        let back_sep = before.len() > 4
            && before[..before.len() - 1].ends_with(b"back")
            && matches!(before[before.len() - 1], b'_' | b'-' | b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
        if before.ends_with(b"back") || before.ends_with(b"(back ") || back_sep {
            return false;
        }
        // `^` then the word, or one non-alphanumeric byte then the word.
        (m == 0 && word_at(0)) || (m < b.len() && !b[m].is_ascii_alphanumeric() && word_at(m + 1))
    })
}

/// Whether the archive already has an image Kavita would take as its cover.
pub fn has_cover(archive: &Path) -> Result<bool> {
    let zip = ZipArchive::new(File::open(archive).with_context(|| format!("opening {}", archive.display()))?)
        .with_context(|| format!("reading {}", archive.display()))?;
    Ok(zip.file_names().filter_map(|n| n.ok()).any(|n| {
        let file = n.rsplit('/').next().unwrap_or(&n);
        !n.starts_with("__MACOSX") && !file.starts_with("._") && is_cover_image(&n)
    }))
}

/// Add `image` as `!cover.<ext>` unless the archive already has a cover.
/// Returns the entry name it added, for [`remove`] on undo.
pub fn embed(archive: &Path, image: &[u8], ext: &str) -> Result<Option<String>> {
    if !supported(archive) || has_cover(archive)? {
        return Ok(None);
    }
    let name = format!("{COVER_STEM}.{ext}");
    rewrite(archive, &name, Some(image), true)?;
    Ok(Some(name))
}

/// Undo [`embed`]: drop the entry it added.
pub fn remove(archive: &Path, name: &str) -> Result<()> {
    rewrite(archive, name, None, false).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    #[test]
    fn matches_kavitas_cover_rule() {
        // Kavita's own ParsingTests.IsCoverImageTest cases (develop, 2026-11-02).
        for (name, want) in [
            ("Love Hina - Special.jpg", false),
            ("folder.jpg", true),
            ("DearS_v01_cover.jpg", true),
            ("DearS_v01_covers.jpg", false),
            ("!cover.jpg", true),
            ("cover.jpg", true),
            ("cover.png", true),
            ("ch1/cover.png", true),
            ("ch1/backcover.png", false),
            ("backcover.png", false),
            ("back_cover.png", false),
            ("LD Blacklands #1 35 (back cover).png", false),
            // And the plan's cases.
            ("discovery.jpg", false),
            ("back cover.jpg", false),
            ("Cover.JPG", true),
            ("cover.txt", false),
            ("001.jpg", false),
        ] {
            assert_eq!(is_cover_image(name), want, "{name}");
        }
    }

    fn cbz(dir: &Path, name: &str, entries: &[&str]) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut z = ZipWriter::new(File::create(&p).unwrap());
        for e in entries {
            z.start_file(*e, SimpleFileOptions::default()).unwrap();
            z.write_all(b"img").unwrap();
        }
        z.finish().unwrap();
        p
    }

    fn names(p: &Path) -> Vec<String> {
        let z = ZipArchive::new(File::open(p).unwrap()).unwrap();
        z.file_names().filter_map(|n| n.ok()).map(|n| n.to_string()).collect()
    }

    #[test]
    fn embeds_first_and_removes() {
        let d = tempfile::tempdir().unwrap();
        let p = cbz(d.path(), "v01.cbz", &["001.jpg", "002.jpg"]);
        let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(embed(&p, b"COVER", "png").unwrap().as_deref(), Some("!cover.png"));
        assert_eq!(names(&p), ["!cover.png", "001.jpg", "002.jpg"]);
        let mut z = ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let mut s = Vec::new();
        z.by_name("!cover.png").unwrap().read_to_end(&mut s).unwrap();
        assert_eq!(s, b"COVER");
        drop(z);
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), mtime);

        // A second run finds the cover it added.
        assert_eq!(embed(&p, b"COVER", "png").unwrap(), None);
        remove(&p, "!cover.png").unwrap();
        assert_eq!(names(&p), ["001.jpg", "002.jpg"]);
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), mtime);
    }

    #[test]
    fn keeps_existing_covers() {
        let d = tempfile::tempdir().unwrap();
        let has = cbz(d.path(), "a.cbz", &["cover.png", "001.jpg"]);
        assert_eq!(embed(&has, b"X", "jpg").unwrap(), None);
        assert_eq!(names(&has), ["cover.png", "001.jpg"]);
        // Not covers by Kavita's rule: a cover is still added.
        for (i, other) in ["discovery.jpg", "back cover.jpg"].into_iter().enumerate() {
            let p = cbz(d.path(), &format!("b{i}.cbz"), &[other, "001.jpg"]);
            assert_eq!(embed(&p, b"X", "jpg").unwrap().as_deref(), Some("!cover.jpg"), "{other}");
        }
        // macOS metadata doesn't count.
        let mac = cbz(d.path(), "c.cbz", &["__MACOSX/._cover.jpg", "001.jpg"]);
        assert!(embed(&mac, b"X", "jpg").unwrap().is_some());
    }
}
