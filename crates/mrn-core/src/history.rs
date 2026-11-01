//! Rename history in SQLite, so any batch can be undone.
//!
//! Every operation is written as `pending` before it touches the disk and
//! marked `done` (with the target's size and mtime) afterwards, so even an
//! interrupted run can be inspected and reverted. A file replaced with
//! `--conflict override` is moved to the trash and its trash path recorded,
//! and an embedded ComicInfo.xml is recorded with the entry it replaced, so
//! both can be reverted too.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::extras::comicinfo;
use crate::ops::{self, Action};
use crate::trash::{self, Trash};

/// What an operation did. `Create` is a file written by extras (nfo, art);
/// `Embed` is ComicInfo.xml written into an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Move,
    Copy,
    Hardlink,
    Symlink,
    Create,
    Embed,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::Move => "move",
            OpKind::Copy => "copy",
            OpKind::Hardlink => "hardlink",
            OpKind::Symlink => "symlink",
            OpKind::Create => "create",
            OpKind::Embed => "embed",
        }
    }

    fn parse(s: &str) -> OpKind {
        match s {
            "copy" => OpKind::Copy,
            "hardlink" => OpKind::Hardlink,
            "symlink" => OpKind::Symlink,
            "create" => OpKind::Create,
            "embed" => OpKind::Embed,
            _ => OpKind::Move,
        }
    }
}

impl From<Action> for OpKind {
    fn from(a: Action) -> Self {
        match a {
            Action::Move => OpKind::Move,
            Action::Copy => OpKind::Copy,
            Action::Hardlink => OpKind::Hardlink,
            Action::Symlink => OpKind::Symlink,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BatchSummary {
    pub id: i64,
    /// Unix seconds.
    pub created: i64,
    pub action: String,
    pub args: String,
    pub status: String,
    pub done: i64,
    pub failed: i64,
}

#[derive(Debug, Clone)]
pub struct OpRecord {
    pub id: i64,
    pub kind: OpKind,
    pub from: Option<PathBuf>,
    pub to: PathBuf,
    pub status: String,
    pub size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub overwrote: bool,
    /// Where the file this op replaced was moved (in the trash).
    pub backup: Option<PathBuf>,
    /// For `Embed`: the ComicInfo.xml the archive had before (`None` if none).
    pub data: Option<Vec<u8>>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct UndoReport {
    pub restored: Vec<(PathBuf, PathBuf)>,
    pub removed: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub warnings: Vec<String>,
}

pub struct History {
    conn: Connection,
    trash: Trash,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS batches (
    id      INTEGER PRIMARY KEY,
    created INTEGER NOT NULL,
    action  TEXT NOT NULL,
    args    TEXT NOT NULL,
    status  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
    id        INTEGER PRIMARY KEY,
    batch     INTEGER NOT NULL REFERENCES batches(id),
    kind      TEXT NOT NULL,
    src       BLOB,
    dst       BLOB NOT NULL,
    status    TEXT NOT NULL,
    size      INTEGER,
    mtime_ns  INTEGER,
    overwrote INTEGER NOT NULL DEFAULT 0,
    error     TEXT,
    backup    BLOB,
    data      BLOB
);
CREATE INDEX IF NOT EXISTS ops_batch ON ops(batch);
CREATE TABLE IF NOT EXISTS dirs (
    batch INTEGER NOT NULL REFERENCES batches(id),
    path  BLOB NOT NULL
);
"#;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn blob(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

fn path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&b))
}

/// (size, mtime in ns) of a path without following symlinks.
fn stat(p: &Path) -> Option<(i64, i64)> {
    let m = fs::symlink_metadata(p).ok()?;
    Some((m.size() as i64, m.mtime() * 1_000_000_000 + m.mtime_nsec()))
}

/// Add columns that older databases lack.
fn migrate(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('ops')")?;
    let columns: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for col in ["backup", "data"] {
        if !columns.iter().any(|c| c == col) {
            conn.execute_batch(&format!("ALTER TABLE ops ADD COLUMN {col} BLOB"))?;
        }
    }
    Ok(())
}

impl History {
    pub fn open(path: &Path) -> Result<History> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening history database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(History {
            conn,
            trash: Trash::from_env(),
        })
    }

    /// Use another trash for replaced files (tests).
    pub fn with_trash(mut self, trash: Trash) -> History {
        self.trash = trash;
        self
    }

    /// Where replaced files go.
    pub fn trash(&self) -> &Trash {
        &self.trash
    }

    pub fn default_path() -> Result<PathBuf> {
        Ok(crate::config::dirs()?.data.join("history.db"))
    }

    pub fn open_default() -> Result<History> {
        Self::open(&Self::default_path()?)
    }

    pub fn begin_batch(&self, action: &str, args: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO batches (created, action, args, status) VALUES (?1, ?2, ?3, 'running')",
            params![now(), action, args],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_batch(&self, batch: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE batches SET status = 'done' WHERE id = ?1",
            params![batch],
        )?;
        Ok(())
    }

    /// Record an operation before it runs.
    pub fn start_op(&self, batch: i64, kind: OpKind, from: Option<&Path>, to: &Path) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO ops (batch, kind, src, dst, status) VALUES (?1, ?2, ?3, ?4, 'pending')",
            params![batch, kind.as_str(), from.map(blob), blob(to)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Mark an operation done and record the target's size and mtime.
    /// `replaced` is where a file that was at the target went.
    pub fn finish_op(&self, op: i64, to: &Path, replaced: Option<&Path>) -> Result<()> {
        let (size, mtime) = stat(to).unzip();
        self.conn.execute(
            "UPDATE ops SET status = 'done', size = ?2, mtime_ns = ?3, overwrote = ?4, backup = ?5
             WHERE id = ?1",
            params![op, size, mtime, replaced.is_some(), replaced.map(blob)],
        )?;
        Ok(())
    }

    /// Mark an `Embed` op done, keeping the ComicInfo.xml it replaced.
    pub fn finish_embed(&self, op: i64, archive: &Path, previous: Option<&[u8]>) -> Result<()> {
        let (size, mtime) = stat(archive).unzip();
        self.conn.execute(
            "UPDATE ops SET status = 'done', size = ?2, mtime_ns = ?3, data = ?4 WHERE id = ?1",
            params![op, size, mtime, previous],
        )?;
        Ok(())
    }

    pub fn fail_op(&self, op: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE ops SET status = 'failed', error = ?2 WHERE id = ?1",
            params![op, error],
        )?;
        Ok(())
    }

    pub fn add_dirs(&self, batch: i64, dirs: &[PathBuf]) -> Result<()> {
        for d in dirs {
            self.conn.execute(
                "INSERT INTO dirs (batch, path) VALUES (?1, ?2)",
                params![batch, blob(d)],
            )?;
        }
        Ok(())
    }

    /// Re-stat a finished op's target (after it was modified in place).
    pub fn restat_op(&self, op: i64, to: &Path) -> Result<()> {
        let (size, mtime) = stat(to).unzip();
        self.conn.execute(
            "UPDATE ops SET size = ?2, mtime_ns = ?3 WHERE id = ?1",
            params![op, size, mtime],
        )?;
        Ok(())
    }

    pub fn batches(&self, limit: usize) -> Result<Vec<BatchSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT b.id, b.created, b.action, b.args, b.status,
                    (SELECT COUNT(*) FROM ops o WHERE o.batch = b.id AND o.status = 'done'
                        AND o.kind != 'embed'),
                    (SELECT COUNT(*) FROM ops o WHERE o.batch = b.id AND o.status = 'failed'
                        AND o.kind != 'embed')
             FROM batches b ORDER BY b.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(BatchSummary {
                id: r.get(0)?,
                created: r.get(1)?,
                action: r.get(2)?,
                args: r.get(3)?,
                status: r.get(4)?,
                done: r.get(5)?,
                failed: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ops(&self, batch: i64) -> Result<Vec<OpRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, src, dst, status, size, mtime_ns, overwrote, error, backup, data
             FROM ops WHERE batch = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![batch], |r| {
            Ok(OpRecord {
                id: r.get(0)?,
                kind: OpKind::parse(&r.get::<_, String>(1)?),
                from: r.get::<_, Option<Vec<u8>>>(2)?.map(path),
                to: path(r.get(3)?),
                status: r.get(4)?,
                size: r.get(5)?,
                mtime_ns: r.get(6)?,
                overwrote: r.get::<_, i64>(7)? != 0,
                error: r.get(8)?,
                backup: r.get::<_, Option<Vec<u8>>>(9)?.map(path),
                data: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest batch that has not been undone.
    pub fn last_undoable(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM batches WHERE status NOT IN ('undone') ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Revert a batch, newest operation first. Files that changed since the
    /// rename are left alone unless `force` is set.
    pub fn undo(&self, batch: i64, force: bool) -> Result<UndoReport> {
        let status: Option<String> = self
            .conn
            .query_row("SELECT status FROM batches WHERE id = ?1", params![batch], |r| r.get(0))
            .optional()?;
        match status.as_deref() {
            None => bail!("no batch #{batch} in history"),
            Some("undone") => bail!("batch #{batch} was already undone"),
            _ => {}
        }

        let mut report = UndoReport::default();
        let mut ops = self.ops(batch)?;
        ops.reverse();
        for i in 0..ops.len() {
            let op = &ops[i];
            if op.status != "done" {
                continue;
            }
            // An embed is reverted in place unless an earlier op of this
            // batch created the archive (it is about to be deleted anyway).
            let rewrite = !ops[i + 1..]
                .iter()
                .any(|o| o.to == op.to && o.status == "done" && !matches!(o.kind, OpKind::Move | OpKind::Embed));
            match self.undo_op(op, force, rewrite, &mut report) {
                Ok(()) => {
                    self.conn.execute(
                        "UPDATE ops SET status = 'undone' WHERE id = ?1",
                        params![op.id],
                    )?;
                    if op.kind == OpKind::Embed {
                        // Earlier ops on this archive recorded it before the
                        // embed; they now expect its current state.
                        let to = op.to.clone();
                        let (size, mtime) = stat(&to).unzip();
                        for earlier in ops[i + 1..].iter_mut().filter(|o| o.to == to) {
                            earlier.size = size;
                            earlier.mtime_ns = mtime;
                            self.restat_op(earlier.id, &to)?;
                        }
                    }
                }
                Err(reason) => report.skipped.push((op.to.clone(), reason)),
            }
        }

        // Remove folders this batch created, deepest first, if now empty.
        let mut stmt = self.conn.prepare("SELECT path FROM dirs WHERE batch = ?1")?;
        let mut dirs: Vec<PathBuf> = stmt
            .query_map(params![batch], |r| r.get::<_, Vec<u8>>(0))?
            .filter_map(|r| r.ok().map(path))
            .collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            let _ = fs::remove_dir(&d);
        }

        let new_status = if report.skipped.is_empty() { "undone" } else { "partial" };
        self.conn.execute(
            "UPDATE batches SET status = ?2 WHERE id = ?1",
            params![batch, new_status],
        )?;
        Ok(report)
    }

    fn undo_op(&self, op: &OpRecord, force: bool, rewrite: bool, report: &mut UndoReport) -> Result<(), String> {
        let Some((size, mtime)) = stat(&op.to) else {
            return Err("no longer exists".into());
        };
        if !force && (Some(size) != op.size || Some(mtime) != op.mtime_ns) {
            return Err("changed since it was renamed (use --force)".into());
        }
        match op.kind {
            OpKind::Move => {
                let from = op.from.as_ref().ok_or("no source path recorded")?;
                if fs::symlink_metadata(from).is_ok() {
                    return Err(format!("{} is occupied", from.display()));
                }
                if let Some(parent) = from.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                ops::move_file(&op.to, from).map_err(|e| e.to_string())?;
                report.restored.push((op.to.clone(), from.clone()));
            }
            OpKind::Copy | OpKind::Hardlink | OpKind::Symlink | OpKind::Create => {
                fs::remove_file(&op.to).map_err(|e| e.to_string())?;
                report.removed.push(op.to.clone());
            }
            OpKind::Embed => {
                if rewrite {
                    comicinfo::restore(&op.to, op.data.as_deref()).map_err(|e| format!("{e:#}"))?;
                }
                return Ok(());
            }
        }
        self.restore_replaced(op, report);
        Ok(())
    }

    /// Put back the file an op replaced, now that its target is free again.
    fn restore_replaced(&self, op: &OpRecord, report: &mut UndoReport) {
        match &op.backup {
            Some(backup) => match trash::restore(backup, &op.to) {
                Ok(()) => report.restored.push((backup.clone(), op.to.clone())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.warnings.push(format!(
                    "the file {} replaced is no longer in the trash ({})",
                    op.to.display(),
                    backup.display()
                )),
                Err(e) => report.warnings.push(format!(
                    "could not restore the replaced {} from {}: {e}",
                    op.to.display(),
                    backup.display()
                )),
            },
            // Batches from before replaced files were kept.
            None if op.overwrote => report.warnings.push(format!(
                "{} had replaced an existing file, which cannot be restored",
                op.to.display()
            )),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::FileOp;

    #[test]
    fn record_and_undo_move() {
        let d = tempfile::tempdir().unwrap();
        let h = History::open(&d.path().join("h.db")).unwrap();
        let src = d.path().join("in/a.mkv");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"x").unwrap();
        let op = FileOp {
            from: src.clone(),
            to: d.path().join("out/Show/a.mkv"),
        };

        let batch = h.begin_batch("move", "test").unwrap();
        let id = h.start_op(batch, OpKind::Move, Some(&op.from), &op.to).unwrap();
        let applied = ops::apply(&op, Action::Move, None).unwrap();
        h.add_dirs(batch, &applied.dirs).unwrap();
        h.finish_op(id, &op.to, None).unwrap();

        let nfo = d.path().join("out/Show/tvshow.nfo");
        fs::write(&nfo, b"<x/>").unwrap();
        let nid = h.start_op(batch, OpKind::Create, None, &nfo).unwrap();
        h.finish_op(nid, &nfo, None).unwrap();
        h.finish_batch(batch).unwrap();

        assert_eq!(h.last_undoable().unwrap(), Some(batch));
        let report = h.undo(batch, false).unwrap();
        assert_eq!(report.restored.len(), 1);
        assert_eq!(report.removed, vec![nfo]);
        assert!(src.exists());
        assert!(!d.path().join("out").exists(), "created dirs are removed");
        assert!(h.undo(batch, false).is_err());
        assert_eq!(h.last_undoable().unwrap(), None);
    }

    #[test]
    fn refuses_changed_files() {
        let d = tempfile::tempdir().unwrap();
        let h = History::open(&d.path().join("h.db")).unwrap();
        let src = d.path().join("a.mkv");
        fs::write(&src, b"x").unwrap();
        let to = d.path().join("b.mkv");
        let batch = h.begin_batch("move", "").unwrap();
        let id = h.start_op(batch, OpKind::Move, Some(&src), &to).unwrap();
        ops::move_file(&src, &to).unwrap();
        h.finish_op(id, &to, None).unwrap();
        fs::write(&to, b"changed!").unwrap();

        let report = h.undo(batch, false).unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert!(to.exists());
    }

    #[test]
    fn undo_restores_replaced_file() {
        let d = tempfile::tempdir().unwrap();
        let h = History::open(&d.path().join("h.db"))
            .unwrap()
            .with_trash(Trash::new(Some(d.path().join("Trash"))));
        let src = d.path().join("new.mkv");
        let dst = d.path().join("Movie.mkv");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        let op = FileOp {
            from: src.clone(),
            to: dst.clone(),
        };

        let batch = h.begin_batch("move", "").unwrap();
        let id = h.start_op(batch, OpKind::Move, Some(&src), &dst).unwrap();
        let applied = ops::apply(&op, Action::Move, Some(h.trash())).unwrap();
        h.finish_op(id, &dst, applied.replaced.as_deref()).unwrap();
        assert_eq!(h.ops(batch).unwrap()[0].backup, applied.replaced);

        let report = h.undo(batch, false).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(fs::read(&src).unwrap(), b"new");
        assert_eq!(fs::read(&dst).unwrap(), b"old");
        assert!(fs::read_dir(d.path().join("Trash/files")).unwrap().next().is_none());
    }

    #[test]
    fn undo_reverts_embedded_comicinfo() {
        use std::io::{Read, Write};
        let d = tempfile::tempdir().unwrap();
        let h = History::open(&d.path().join("h.db")).unwrap();
        let src = d.path().join("in.cbz");
        let mut z = zip::ZipWriter::new(fs::File::create(&src).unwrap());
        z.start_file("001.jpg", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"img").unwrap();
        z.finish().unwrap();
        let to = d.path().join("Comic 001.cbz");

        let batch = h.begin_batch("move", "").unwrap();
        let id = h.start_op(batch, OpKind::Move, Some(&src), &to).unwrap();
        ops::move_file(&src, &to).unwrap();
        h.finish_op(id, &to, None).unwrap();
        let eid = h.start_op(batch, OpKind::Embed, None, &to).unwrap();
        let prev = comicinfo::inject(&to, "<ComicInfo/>").unwrap();
        assert_eq!(prev, None);
        h.finish_embed(eid, &to, prev.as_deref()).unwrap();
        assert_eq!(h.batches(1).unwrap()[0].done, 1, "embeds are not counted as files");

        let report = h.undo(batch, false).unwrap();
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let mut zin = zip::ZipArchive::new(fs::File::open(&src).unwrap()).unwrap();
        assert_eq!(zin.len(), 1);
        let mut img = String::new();
        zin.by_name("001.jpg").unwrap().read_to_string(&mut img).unwrap();
        assert_eq!(img, "img");
    }

    #[test]
    fn migrates_old_databases() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("h.db");
        let old = Connection::open(&db).unwrap();
        old.execute_batch(
            "CREATE TABLE ops (id INTEGER PRIMARY KEY, batch INTEGER NOT NULL, kind TEXT NOT NULL,
             src BLOB, dst BLOB NOT NULL, status TEXT NOT NULL, size INTEGER, mtime_ns INTEGER,
             overwrote INTEGER NOT NULL DEFAULT 0, error TEXT);",
        )
        .unwrap();
        drop(old);
        let h = History::open(&db).unwrap();
        let batch = h.begin_batch("move", "").unwrap();
        assert!(h.ops(batch).unwrap().is_empty());
    }
}
