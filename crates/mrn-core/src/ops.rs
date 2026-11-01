//! Rename planning and the file system operations that carry it out.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    #[default]
    Move,
    Copy,
    Hardlink,
    Symlink,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Move => "move",
            Action::Copy => "copy",
            Action::Hardlink => "hardlink",
            Action::Symlink => "symlink",
        }
    }

    /// Imperative and past-tense verbs for messages: ("Move", "Moved").
    /// A move without a library root is a rename.
    pub fn verbs(self, in_place: bool) -> (&'static str, &'static str) {
        match self {
            Action::Move if in_place => ("Rename", "Renamed"),
            Action::Move => ("Move", "Moved"),
            Action::Copy => ("Copy", "Copied"),
            Action::Hardlink => ("Hardlink", "Hardlinked"),
            Action::Symlink => ("Symlink", "Symlinked"),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "move" | "rename" | "mv" => Ok(Action::Move),
            "copy" | "cp" => Ok(Action::Copy),
            "hardlink" | "link" | "ln" => Ok(Action::Hardlink),
            "symlink" | "ln-s" => Ok(Action::Symlink),
            o => Err(format!("unknown action '{o}' (move, copy, hardlink, symlink)")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conflict {
    #[default]
    Skip,
    Override,
    Index,
}

impl FromStr for Conflict {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "skip" => Ok(Conflict::Skip),
            "override" | "overwrite" => Ok(Conflict::Override),
            "index" => Ok(Conflict::Index),
            o => Err(format!("unknown conflict mode '{o}' (skip, override, index)")),
        }
    }
}

/// One file to move from `from` to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOp {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemStatus {
    Ready,
    /// Ready, and will replace an existing file.
    Overwrite,
    Skip(String),
}

impl ItemStatus {
    pub fn is_runnable(&self) -> bool {
        !matches!(self, ItemStatus::Skip(_))
    }
}

/// A requested rename before conflicts are resolved.
#[derive(Debug, Clone)]
pub struct Request {
    /// Caller's index for this file (the pipeline maps it back to metadata).
    pub tag: usize,
    pub from: PathBuf,
    /// Full destination path including extension.
    pub to: PathBuf,
    /// Sidecar files and their suffix after the main stem (`.en.srt`).
    pub sidecars: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub tag: usize,
    pub main: FileOp,
    pub sidecars: Vec<FileOp>,
    pub status: ItemStatus,
}

impl PlanItem {
    pub fn ops(&self) -> impl Iterator<Item = &FileOp> {
        std::iter::once(&self.main).chain(self.sidecars.iter())
    }
}

/// Resolve target conflicts (on disk and within the batch) for a list of
/// requests.
pub fn build_plan(requests: Vec<Request>, conflict: Conflict) -> Vec<PlanItem> {
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut plan = Vec::with_capacity(requests.len());

    for req in requests {
        let make = |main_to: &Path| -> Vec<FileOp> {
            let mut ops = vec![FileOp {
                from: req.from.clone(),
                to: main_to.to_path_buf(),
            }];
            for (from, suffix) in &req.sidecars {
                ops.push(FileOp {
                    from: from.clone(),
                    to: sidecar_target(main_to, suffix),
                });
            }
            ops
        };

        let mut ops = make(&req.to);
        let mut status = ItemStatus::Ready;

        if ops.iter().all(|o| o.from == o.to) {
            status = ItemStatus::Skip("already named correctly".into());
        } else {
            let check = |ops: &[FileOp]| -> Option<Clash> {
                ops.iter().find_map(|o| {
                    if claimed.contains(&o.to) {
                        Some(Clash::Batch)
                    } else if o.from != o.to && exists_as_other_file(&o.from, &o.to) {
                        Some(Clash::Disk)
                    } else {
                        None
                    }
                })
            };
            match (check(&ops), conflict) {
                (None, _) => {}
                (Some(Clash::Batch), Conflict::Skip | Conflict::Override) => {
                    status = ItemStatus::Skip("another file in this batch has the same target".into())
                }
                (Some(Clash::Disk), Conflict::Skip) => {
                    status = ItemStatus::Skip("target already exists".into())
                }
                (Some(Clash::Disk), Conflict::Override) => status = ItemStatus::Overwrite,
                (Some(_), Conflict::Index) => {
                    let mut found = false;
                    for n in 1..100 {
                        let candidate = indexed(&req.to, n);
                        let attempt = make(&candidate);
                        if check(&attempt).is_none() {
                            ops = attempt;
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        status = ItemStatus::Skip("no free indexed name".into());
                    }
                }
            }
        }

        if status.is_runnable() {
            for o in &ops {
                claimed.insert(o.to.clone());
            }
        }
        let mut iter = ops.into_iter();
        let main = iter.next().unwrap();
        plan.push(PlanItem {
            tag: req.tag,
            main,
            sidecars: iter.collect(),
            status,
        });
    }
    plan
}

enum Clash {
    Batch,
    Disk,
}

/// True if `to` exists and is not the very same file as `from` (a case-only
/// rename or an existing hard link is not a conflict).
fn exists_as_other_file(from: &Path, to: &Path) -> bool {
    let Ok(to_meta) = fs::symlink_metadata(to) else {
        return false;
    };
    match fs::symlink_metadata(from) {
        Ok(from_meta) => !(from_meta.dev() == to_meta.dev() && from_meta.ino() == to_meta.ino()),
        Err(_) => true,
    }
}

/// `dir/Name.mkv` + `.en.srt` → `dir/Name.en.srt`
pub fn sidecar_target(main_to: &Path, suffix: &str) -> PathBuf {
    let stem = main_to
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    main_to.with_file_name(format!("{stem}{suffix}"))
}

/// `Name.mkv` → `Name (2).mkv`
fn indexed(path: &Path, n: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
        None => format!("{stem} ({n})"),
    };
    path.with_file_name(name)
}

/// Directories created for a target, outermost first.
pub fn create_parent_dirs(to: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(parent) = to.parent() else {
        return Ok(vec![]);
    };
    let mut missing: Vec<PathBuf> = parent
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .map(Path::to_path_buf)
        .collect();
    missing.reverse();
    fs::create_dir_all(parent)?;
    Ok(missing)
}

/// Carry out one file operation. Creates parent folders and returns the ones
/// it created so they can be removed again on undo.
pub fn apply(op: &FileOp, action: Action, overwrite: bool) -> io::Result<Vec<PathBuf>> {
    let created = create_parent_dirs(&op.to)?;
    if overwrite {
        match fs::symlink_metadata(&op.to) {
            Ok(m) if m.is_dir() => {
                return Err(io::Error::other(format!(
                    "refusing to overwrite directory {}",
                    op.to.display()
                )));
            }
            // rename(2) replaces atomically; the other actions need a free name.
            Ok(_) if action != Action::Move => fs::remove_file(&op.to)?,
            _ => {}
        }
    } else if fs::symlink_metadata(&op.to).is_ok() && !same_file(&op.from, &op.to) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", op.to.display()),
        ));
    }

    match action {
        Action::Move => move_file(&op.from, &op.to)?,
        Action::Copy => copy_file(&op.from, &op.to)?,
        Action::Hardlink => fs::hard_link(&op.from, &op.to)?,
        Action::Symlink => {
            let abs = fs::canonicalize(&op.from)?;
            std::os::unix::fs::symlink(abs, &op.to)?
        }
    }
    Ok(created)
}

/// Same inode, without following a symlink at `b` (a link pointing at `a` is
/// a different file).
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Rename, falling back to copy + fsync + remove across file systems.
pub fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            copy_file(from, to)?;
            fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

/// Copy through a temporary file so a crash never leaves a half-written
/// target. Keeps the modification time.
pub fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    let tmp = to.with_file_name(format!(
        ".{}.mrn-part",
        to.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        fs::copy(from, &tmp)?;
        let file = fs::OpenOptions::new().write(true).open(&tmp)?;
        if let Ok(mtime) = fs::metadata(from).and_then(|m| m.modified()) {
            let _ = file.set_modified(mtime);
        }
        file.sync_all()?;
        fs::rename(&tmp, to)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Remove `dir` and its now-empty parents, stopping at `stop` (exclusive).
pub fn prune_empty_dirs(dir: &Path, stop: &Path) {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d == stop || !d.starts_with(stop) {
            break;
        }
        if fs::remove_dir(d).is_err() {
            break;
        }
        cur = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(tag: usize, from: &Path, to: &Path) -> Request {
        Request {
            tag,
            from: from.into(),
            to: to.into(),
            sidecars: vec![],
        }
    }

    #[test]
    fn conflicts() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.mkv");
        let b = d.path().join("b.mkv");
        let existing = d.path().join("Target.mkv");
        for p in [&a, &b, &existing] {
            fs::write(p, p.to_string_lossy().as_bytes()).unwrap();
        }
        let target = d.path().join("Target.mkv");

        let plan = build_plan(vec![req(0, &a, &target), req(1, &b, &target)], Conflict::Skip);
        assert!(matches!(plan[0].status, ItemStatus::Skip(_)));

        let plan = build_plan(vec![req(0, &a, &target), req(1, &b, &target)], Conflict::Index);
        assert_eq!(plan[0].main.to, d.path().join("Target (1).mkv"));
        assert_eq!(plan[1].main.to, d.path().join("Target (2).mkv"));

        let plan = build_plan(vec![req(0, &a, &target), req(1, &b, &target)], Conflict::Override);
        assert_eq!(plan[0].status, ItemStatus::Overwrite);
        assert!(matches!(plan[1].status, ItemStatus::Skip(_)));

        let plan = build_plan(vec![req(0, &a, &a)], Conflict::Skip);
        assert!(matches!(plan[0].status, ItemStatus::Skip(_)));
    }

    #[test]
    fn sidecars_follow_main() {
        assert_eq!(
            sidecar_target(Path::new("/x/Show - S01E01.mkv"), ".en.srt"),
            PathBuf::from("/x/Show - S01E01.en.srt")
        );
    }

    #[test]
    fn apply_actions() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src.mkv");
        fs::write(&src, b"data").unwrap();

        let op = FileOp {
            from: src.clone(),
            to: d.path().join("a/b/copy.mkv"),
        };
        let created = apply(&op, Action::Copy, false).unwrap();
        assert_eq!(created, vec![d.path().join("a"), d.path().join("a/b")]);
        assert_eq!(fs::read(&op.to).unwrap(), b"data");

        let link = FileOp {
            from: src.clone(),
            to: d.path().join("link.mkv"),
        };
        apply(&link, Action::Symlink, false).unwrap();
        assert!(fs::symlink_metadata(&link.to).unwrap().file_type().is_symlink());

        // Existing target without overwrite fails.
        assert!(apply(&link, Action::Copy, false).is_err());

        let mv = FileOp {
            from: src.clone(),
            to: d.path().join("moved.mkv"),
        };
        apply(&mv, Action::Move, false).unwrap();
        assert!(!src.exists());
        assert!(mv.to.exists());
    }
}
