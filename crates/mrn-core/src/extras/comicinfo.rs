//! ComicInfo.xml (Anansi schema v2) for Komga/Kavita, embedded into `.cbz`.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use super::nfo::Xml;
use crate::MediaKind;
use crate::format::template::format_number;
use crate::parse::ParsedInfo;
use crate::providers::{Candidate, Issue};

const ENTRY: &str = "ComicInfo.xml";

pub fn build(kind: MediaKind, c: &Candidate, info: &ParsedInfo, issue: Option<&Issue>) -> String {
    let mut x = Xml::new(
        "ComicInfo",
        " xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"",
    );
    let number = match kind {
        MediaKind::Comic => info.issue,
        _ => info.chapter,
    };
    let (year, month, day) = issue
        .and_then(|i| i.cover_date.as_deref())
        .map(split_date)
        .unwrap_or((None, None, None));
    x.el("Title", issue.and_then(|i| i.title.as_ref()))
        .el("Series", Some(&c.title))
        .el("Number", number.map(|n| format_number(n, 0)))
        .el("Volume", info.volume.map(|v| format_number(v, 0)))
        .el(
            "Summary",
            issue.and_then(|i| i.summary.as_ref()).or(c.overview.as_ref()),
        )
        .el("Year", year.or(c.year.map(|y| y.to_string())))
        .el("Month", month)
        .el("Day", day)
        .el("Publisher", c.publisher.as_ref())
        .el("Genre", (!c.genres.is_empty()).then(|| c.genres.join(", ")))
        .el("Web", c.url.as_ref());
    if kind == MediaKind::Manga {
        x.el("Manga", Some("YesAndRightToLeft"));
    }
    x.finish()
}

fn split_date(d: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut parts = d.split('-').map(|p| p.trim_start_matches('0').to_string());
    let y = parts.next().filter(|p| !p.is_empty());
    let m = parts.next().filter(|p| !p.is_empty());
    let day = parts.next().filter(|p| !p.is_empty());
    (y, m, day)
}

/// Whether ComicInfo can be embedded into this file.
pub fn supported(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cbz") || e.eq_ignore_ascii_case("zip"))
}

/// Write `xml` as ComicInfo.xml into the archive, replacing any existing one.
/// Returns the entry it replaced, so the change can be undone with
/// [`restore`].
pub fn inject(archive: &Path, xml: &str) -> Result<Option<Vec<u8>>> {
    rewrite(archive, Some(xml.as_bytes()))
}

/// Undo [`inject`]: put back the `previous` ComicInfo.xml, or remove the
/// entry if the archive had none.
pub fn restore(archive: &Path, previous: Option<&[u8]>) -> Result<()> {
    rewrite(archive, previous).map(drop)
}

/// Replace the ComicInfo.xml entry with `content` (or drop it) and return the
/// old entry. Other entries are copied without recompression, the archive is
/// replaced atomically and keeps its modification time.
fn rewrite(archive: &Path, content: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
    if !supported(archive) {
        bail!("ComicInfo can only be embedded in .cbz/.zip archives");
    }
    let src = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mtime = src.metadata().and_then(|m| m.modified()).ok();
    let mut zin = ZipArchive::new(src).with_context(|| format!("reading {}", archive.display()))?;

    let tmp = archive.with_file_name(format!(
        ".{}.mrn-part",
        archive.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| -> Result<Option<Vec<u8>>> {
        let mut previous = None;
        let mut zout = ZipWriter::new(File::create(&tmp)?);
        for i in 0..zin.len() {
            let is_info = zin.by_index_raw(i)?.name().is_ok_and(|n| n.eq_ignore_ascii_case(ENTRY));
            if is_info {
                let mut old = Vec::new();
                zin.by_index(i)?.read_to_end(&mut old)?;
                previous = Some(old);
                continue;
            }
            zout.raw_copy_file(zin.by_index_raw(i)?)?;
        }
        if let Some(content) = content {
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .last_modified_time(zip_now());
            zout.start_file(ENTRY, options)?;
            zout.write_all(content)?;
        }
        let file = zout.finish()?;
        if let Some(t) = mtime {
            let _ = file.set_modified(t);
        }
        file.sync_all()?;
        Ok(previous)
    })();
    match result {
        Ok(previous) => {
            std::fs::rename(&tmp, archive)?;
            Ok(previous)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.context(format!("rewriting {}", archive.display())))
        }
    }
}

/// The current UTC time as a zip timestamp. The zip crate's `time` feature
/// would do this too, but pulls in a date library for one call.
fn zip_now() -> DateTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    zip_time(secs)
}

/// Unix seconds (UTC) → zip timestamp. Falls back to 1980-01-01 outside the
/// range zip can store (1980..=2107).
fn zip_time(secs: u64) -> DateTime {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days, Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, m, s) = ((rem / 3600) as u8, (rem / 60 % 60) as u8, (rem % 60) as u8);
    u16::try_from(year)
        .ok()
        .and_then(|y| DateTime::from_date_and_time(y, month, day, h, m, s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_injects() {
        let d = tempfile::tempdir().unwrap();
        let cbz = d.path().join("Saga #001.cbz");
        {
            let mut z = ZipWriter::new(File::create(&cbz).unwrap());
            z.start_file("001.jpg", SimpleFileOptions::default()).unwrap();
            z.write_all(b"jpegdata").unwrap();
            z.start_file("comicinfo.xml", SimpleFileOptions::default()).unwrap();
            z.write_all(b"<old/>").unwrap();
            z.finish().unwrap();
        }
        let c = Candidate {
            title: "Saga".into(),
            year: Some(2012),
            publisher: Some("Image".into()),
            ..Default::default()
        };
        let info = ParsedInfo {
            issue: Some(1.0),
            ..Default::default()
        };
        let issue = Issue {
            title: Some("Chapter One".into()),
            cover_date: Some("2012-03-01".into()),
            ..Default::default()
        };
        let xml = build(MediaKind::Comic, &c, &info, Some(&issue));
        assert!(xml.contains("<Number>1</Number>"));
        assert!(xml.contains("<Month>3</Month>"));
        assert!(!xml.contains("<Manga>"));

        assert_eq!(inject(&cbz, &xml).unwrap().as_deref(), Some(&b"<old/>"[..]));
        let mut z = ZipArchive::new(File::open(&cbz).unwrap()).unwrap();
        let names: Vec<String> = z.file_names().filter_map(|n| n.ok()).map(|n| n.to_string()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let mut s = String::new();
        let mut entry = z.by_name(ENTRY).unwrap();
        assert!(entry.last_modified().is_some_and(|t| t.year() >= 2025), "entry is dated 1980");
        entry.read_to_string(&mut s).unwrap();
        drop(entry);
        assert!(s.contains("<Series>Saga</Series>"));
        let mut img = Vec::new();
        z.by_name("001.jpg").unwrap().read_to_end(&mut img).unwrap();
        assert_eq!(img, b"jpegdata");
        drop(z);

        restore(&cbz, Some(b"<old/>")).unwrap();
        let mut z = ZipArchive::new(File::open(&cbz).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name(ENTRY).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "<old/>");
        drop(z);
        restore(&cbz, None).unwrap();
        assert_eq!(ZipArchive::new(File::open(&cbz).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn zip_time_from_unix() {
        let t = zip_time(1_793_534_445); // 2026-11-01 12:00:45 UTC
        assert_eq!((t.year(), t.month(), t.day()), (2026, 11, 1));
        assert_eq!((t.hour(), t.minute(), t.second()), (12, 0, 44)); // zip stores even seconds
        let leap = zip_time(951_782_400); // 2000-02-29 00:00:00
        assert_eq!((leap.year(), leap.month(), leap.day()), (2000, 2, 29));
        assert_eq!(zip_time(0).year(), 1980); // before the zip epoch
    }
}
