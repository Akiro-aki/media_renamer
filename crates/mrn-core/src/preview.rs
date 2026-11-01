//! A display-ready summary of an [`Analysis`] and its plan, shared by the CLI
//! and the GUI so both show the same thing.

use std::path::{Path, PathBuf};

use crate::MediaKind;
use crate::ops::{ItemStatus, PlanItem};
use crate::pipeline::Analysis;

#[derive(Debug, Clone, PartialEq)]
pub enum RowState {
    Ready,
    /// Ready, and will replace an existing file.
    Overwrite,
    /// Named, but the plan skips it (target exists, duplicate target, …).
    Skipped(String),
    /// No target: no match, lookup failed, or the template failed.
    Unmatched(String),
}

impl RowState {
    pub fn is_runnable(&self) -> bool {
        matches!(self, RowState::Ready | RowState::Overwrite)
    }
}

#[derive(Debug, Clone)]
pub struct PreviewRow {
    /// Index into [`Analysis::items`].
    pub item: usize,
    pub kind: MediaKind,
    pub source: PathBuf,
    /// Source relative to the scanned folder.
    pub from: String,
    /// Target relative to the library root (or the source folder when renaming in place).
    pub to: Option<String>,
    pub target: Option<PathBuf>,
    pub sidecars: usize,
    /// `Title (Year)` of the match.
    pub matched: Option<String>,
    /// `tmdb:1399`.
    pub matched_id: Option<String>,
    pub score: Option<f64>,
    pub state: RowState,
}

#[derive(Debug, Clone, Default)]
pub struct Preview {
    /// Planned rows first (in plan order), then files that got no name.
    pub rows: Vec<PreviewRow>,
    pub ready: usize,
    pub skipped: usize,
    pub unmatched: usize,
    /// No file goes into a library root: a plain rename.
    pub in_place: bool,
}

impl Preview {
    pub fn build(analysis: &Analysis, plan: &[PlanItem]) -> Preview {
        let mut p = Preview {
            in_place: analysis.items.iter().all(|i| !i.organized),
            ..Default::default()
        };
        for item in plan {
            let meta = &analysis.items[item.tag];
            let base = meta
                .output_root
                .clone()
                .or_else(|| item.main.from.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            let state = match &item.status {
                ItemStatus::Ready => RowState::Ready,
                ItemStatus::Overwrite => RowState::Overwrite,
                ItemStatus::Skip(reason) => RowState::Skipped(reason.clone()),
            };
            match state {
                RowState::Skipped(_) => p.skipped += 1,
                _ => p.ready += 1,
            }
            p.rows.push(PreviewRow {
                item: item.tag,
                kind: meta.kind,
                source: item.main.from.clone(),
                from: relative(&item.main.from, &meta.file.root),
                to: Some(relative(&item.main.to, &base)),
                target: Some(item.main.to.clone()),
                sidecars: item.sidecars.len(),
                matched: meta.matched.as_ref().map(|m| m.candidate.label()),
                matched_id: meta
                    .matched
                    .as_ref()
                    .map(|m| format!("{}:{}", m.candidate.source(), m.candidate.id)),
                score: meta.matched.as_ref().map(|m| m.score),
                state,
            });
        }
        for (i, meta) in analysis.items.iter().enumerate() {
            let Err(reason) = &meta.target else { continue };
            p.unmatched += 1;
            p.rows.push(PreviewRow {
                item: i,
                kind: meta.kind,
                source: meta.file.path.clone(),
                from: relative(&meta.file.path, &meta.file.root),
                to: None,
                target: None,
                sidecars: meta.file.sidecars.len(),
                matched: None,
                matched_id: None,
                score: None,
                state: RowState::Unmatched(reason.clone()),
            });
        }
        p
    }

    /// One-line count, e.g. "3 ready, 1 skipped, 2 not matched".
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} ready", self.ready)];
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.unmatched > 0 {
            parts.push(format!("{} not matched", self.unmatched));
        }
        parts.join(", ")
    }
}

/// Show `p` relative to `base` when it is inside it.
pub fn relative(p: &Path, base: &Path) -> String {
    p.strip_prefix(base)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}

/// "5 min ago", "3 days ago".
pub fn ago(unix_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix_secs);
    let d = (now - unix_secs).max(0);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86400 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}
