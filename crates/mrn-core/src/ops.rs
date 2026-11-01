//! Rename planning and the file system operations that carry it out.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::trash::{self, Trash};

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
/// requests, and order the items so they can run one after another.
///
/// With [`Action::Move`], a target that another item in the batch moves away
/// (A→B while B→C) is not a conflict: the item that frees it runs first.
/// Items that would trade names in a cycle (A→B and B→A) are skipped.
pub fn build_plan(requests: Vec<Request>, conflict: Conflict, action: Action) -> Vec<PlanItem> {
    // Paths assumed to be moved away. Start optimistic and drop every path
    // whose item turns out not to run, until the assumption holds.
    let mut vacating: HashSet<PathBuf> = HashSet::new();
    if action == Action::Move {
        for r in &requests {
            vacating.insert(r.from.clone());
            vacating.extend(r.sidecars.iter().map(|(p, _)| p.clone()));
        }
    }
    let mut blocked: HashMap<usize, &'static str> = HashMap::new();
    loop {
        let plan = plan_once(&requests, conflict, &vacating, &blocked);
        let vacated: HashSet<&PathBuf> = plan
            .iter()
            .filter(|p| p.status.is_runnable())
            .flat_map(PlanItem::ops)
            .filter(|o| o.from != o.to)
            .map(|o| &o.from)
            .collect();
        if vacating.iter().any(|p| !vacated.contains(p)) {
            let keep: HashSet<PathBuf> = vacating.iter().filter(|p| vacated.contains(p)).cloned().collect();
            vacating = keep;
            continue;
        }
        match run_order(&plan) {
            Ok(order) => {
                let mut slots: Vec<Option<PlanItem>> = plan.into_iter().map(Some).collect();
                return order.into_iter().filter_map(|i| slots[i].take()).collect();
            }
            Err(cycle) => {
                for i in cycle {
                    blocked.insert(i, "would trade names with another file in this batch");
                }
            }
        }
    }
}

/// One planning pass. `vacating` holds paths other items move away;
/// `blocked` forces items (by request index) to be skipped.
fn plan_once(
    requests: &[Request],
    conflict: Conflict,
    vacating: &HashSet<PathBuf>,
    blocked: &HashMap<usize, &'static str>,
) -> Vec<PlanItem> {
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut plan = Vec::with_capacity(requests.len());

    for (index, req) in requests.iter().enumerate() {
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
        } else if let Some(reason) = blocked.get(&index) {
            status = ItemStatus::Skip((*reason).into());
        } else {
            let check = |ops: &[FileOp]| -> Option<Clash> {
                ops.iter().find_map(|o| {
                    if claimed.contains(&o.to) {
                        Some(Clash::Batch)
                    } else if o.from != o.to
                        && !vacating.contains(&o.to)
                        && exists_as_other_file(&o.from, &o.to)
                    {
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

/// Indices of `plan` in an order where every item runs after the items that
/// free its targets, otherwise keeping the original order. Fails with the
/// items caught in a cycle.
fn run_order(plan: &[PlanItem]) -> Result<Vec<usize>, Vec<usize>> {
    let runnable = |i: usize| plan[i].status.is_runnable();
    let mut freed_by: HashMap<&Path, usize> = HashMap::new();
    for (i, item) in plan.iter().enumerate().filter(|(i, _)| runnable(*i)) {
        for o in item.ops().filter(|o| o.from != o.to) {
            freed_by.insert(&o.from, i);
        }
    }
    // after[y] = items that must wait for y.
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); plan.len()];
    let mut waits = vec![0usize; plan.len()];
    for (x, item) in plan.iter().enumerate().filter(|(i, _)| runnable(*i)) {
        let mut deps: Vec<usize> = item
            .ops()
            .filter_map(|o| freed_by.get(o.to.as_path()).copied())
            .filter(|&y| y != x)
            .collect();
        deps.sort_unstable();
        deps.dedup();
        for y in deps {
            after[y].push(x);
            waits[x] += 1;
        }
    }
    let mut ready: BTreeSet<usize> = (0..plan.len()).filter(|&i| waits[i] == 0).collect();
    let mut order = Vec::with_capacity(plan.len());
    while let Some(i) = ready.pop_first() {
        order.push(i);
        for &x in &after[i] {
            waits[x] -= 1;
            if waits[x] == 0 {
                ready.insert(x);
            }
        }
    }
    if order.len() == plan.len() {
        Ok(order)
    } else {
        Err((0..plan.len()).filter(|&i| waits[i] > 0).collect())
    }
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

/// What [`apply`] did besides the operation itself.
#[derive(Debug, Default)]
pub struct Applied {
    /// Folders created for the target, outermost first.
    pub dirs: Vec<PathBuf>,
    /// Where the file that was at the target went (in the trash).
    pub replaced: Option<PathBuf>,
}

/// Carry out one file operation. Creates parent folders and returns the ones
/// it created so they can be removed again on undo. With `replace`, a file
/// already at the target is moved to that trash first (and put back if the
/// operation fails); without it, an existing target is an error.
pub fn apply(op: &FileOp, action: Action, replace: Option<&Trash>) -> io::Result<Applied> {
    let dirs = create_parent_dirs(&op.to)?;
    let mut replaced = None;
    if let Ok(meta) = fs::symlink_metadata(&op.to)
        && !same_file(&op.from, &op.to)
    {
        match replace {
            Some(_) if meta.is_dir() => {
                return Err(io::Error::other(format!(
                    "refusing to overwrite directory {}",
                    op.to.display()
                )));
            }
            Some(trash) => replaced = Some(trash.put(&op.to)?),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", op.to.display()),
                ));
            }
        }
    }

    let result = match action {
        Action::Move => move_file(&op.from, &op.to),
        Action::Copy => copy_file(&op.from, &op.to),
        Action::Hardlink => fs::hard_link(&op.from, &op.to),
        Action::Symlink => {
            fs::canonicalize(&op.from).and_then(|abs| std::os::unix::fs::symlink(abs, &op.to))
        }
    };
    if let Err(e) = result {
        if let Some(t) = &replaced {
            let _ = trash::restore(t, &op.to);
        }
        return Err(e);
    }
    Ok(Applied { dirs, replaced })
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

        let plan = build_plan(vec![req(0, &a, &target), req(1, &b, &target)], Conflict::Skip, Action::Move);
        assert!(matches!(plan[0].status, ItemStatus::Skip(_)));

        let plan = build_plan(vec![req(0, &a, &target), req(1, &b, &target)], Conflict::Index, Action::Move);
        assert_eq!(plan[0].main.to, d.path().join("Target (1).mkv"));
        assert_eq!(plan[1].main.to, d.path().join("Target (2).mkv"));

        let plan = build_plan(
            vec![req(0, &a, &target), req(1, &b, &target)],
            Conflict::Override,
            Action::Move,
        );
        assert_eq!(plan[0].status, ItemStatus::Overwrite);
        assert!(matches!(plan[1].status, ItemStatus::Skip(_)));

        let plan = build_plan(vec![req(0, &a, &a)], Conflict::Skip, Action::Move);
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
        let applied = apply(&op, Action::Copy, None).unwrap();
        assert_eq!(applied.dirs, vec![d.path().join("a"), d.path().join("a/b")]);
        assert_eq!(fs::read(&op.to).unwrap(), b"data");

        let link = FileOp {
            from: src.clone(),
            to: d.path().join("link.mkv"),
        };
        apply(&link, Action::Symlink, None).unwrap();
        assert!(fs::symlink_metadata(&link.to).unwrap().file_type().is_symlink());

        // Existing target without overwrite fails.
        assert!(apply(&link, Action::Copy, None).is_err());

        let mv = FileOp {
            from: src.clone(),
            to: d.path().join("moved.mkv"),
        };
        apply(&mv, Action::Move, None).unwrap();
        assert!(!src.exists());
        assert!(mv.to.exists());
    }

    #[test]
    fn replace_goes_to_trash() {
        let d = tempfile::tempdir().unwrap();
        let trash = Trash::new(Some(d.path().join("Trash")));
        let src = d.path().join("new.mkv");
        let dst = d.path().join("Movie.mkv");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        let op = FileOp {
            from: src.clone(),
            to: dst.clone(),
        };
        let applied = apply(&op, Action::Move, Some(&trash)).unwrap();
        let replaced = applied.replaced.unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert_eq!(fs::read(&replaced).unwrap(), b"old");

        // A failed operation puts the replaced file back.
        fs::write(&src, b"x").unwrap();
        fs::remove_file(&src).unwrap();
        assert!(apply(&op, Action::Move, Some(&trash)).is_err());
        assert_eq!(fs::read(&dst).unwrap(), b"new");
    }

    #[test]
    fn chain_renames_run_in_order() {
        let d = tempfile::tempdir().unwrap();
        let p = |n: &str| d.path().join(n);
        for n in ["a", "b", "x", "y"] {
            fs::write(p(n), n).unwrap();
        }
        // a→b while b→c: b moves first.
        let plan = build_plan(vec![req(0, &p("a"), &p("b")), req(1, &p("b"), &p("c"))], Conflict::Skip, Action::Move);
        assert!(plan.iter().all(|i| i.status == ItemStatus::Ready), "{plan:?}");
        assert_eq!(plan.iter().map(|i| i.tag).collect::<Vec<_>>(), vec![1, 0]);
        for item in &plan {
            apply(&item.main, Action::Move, None).unwrap();
        }
        assert_eq!(fs::read(p("b")).unwrap(), b"a");
        assert_eq!(fs::read(p("c")).unwrap(), b"b");

        // With copy, the source stays, so it is still a conflict.
        let plan = build_plan(vec![req(0, &p("x"), &p("y")), req(1, &p("y"), &p("z"))], Conflict::Skip, Action::Copy);
        assert!(matches!(plan[0].status, ItemStatus::Skip(_)));

        // If the item that would free the target is skipped, the target stays taken.
        let plan = build_plan(
            vec![req(0, &p("x"), &p("y")), req(1, &p("y"), &p("b"))],
            Conflict::Skip,
            Action::Move,
        );
        assert!(plan.iter().all(|i| matches!(i.status, ItemStatus::Skip(_))), "{plan:?}");

        // A swap is skipped rather than half done.
        let plan = build_plan(vec![req(0, &p("x"), &p("y")), req(1, &p("y"), &p("x"))], Conflict::Override, Action::Move);
        assert!(plan.iter().all(|i| matches!(i.status, ItemStatus::Skip(_))), "{plan:?}");
    }
}
