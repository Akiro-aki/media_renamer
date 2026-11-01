//! Rename history in SQLite, so any batch can be undone.
//!
//! Every operation is written as `pending` before it touches the disk and
//! marked `done` (with the target's size and mtime) afterwards, so even an
//! interrupted run can be inspected and reverted.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::ops::{self, Action};

/// What an operation did. `Create` is a file written by extras (nfo, art).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Move,
    Copy,
    Hardlink,
    Symlink,
    Create,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::Move => "move",
            OpKind::Copy => "copy",
            OpKind::Hardlink => "hardlink",
            OpKind::Symlink => "symlink",
            OpKind::Create => "create",
        }
    }

    fn parse(s: &str) -> OpKind {
        match s {
            "copy" => OpKind::Copy,
            "hardlink" => OpKind::Hardlink,
            "symlink" => OpKind::Symlink,
            "create" => OpKind::Create,
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
    error     TEXT
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
        Ok(History { conn })
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

    /// Mark an operation done. Call after any post-processing of the target
    /// (e.g. ComicInfo injection) so the recorded size/mtime are final.
    pub fn finish_op(&self, op: i64, to: &Path, overwrote: bool) -> Result<()> {
        let (size, mtime) = stat(to).unzip();
        self.conn.execute(
            "UPDATE ops SET status = 'done', size = ?2, mtime_ns = ?3, overwrote = ?4 WHERE id = ?1",
            params![op, size, mtime, overwrote],
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
                    (SELECT COUNT(*) FROM ops o WHERE o.batch = b.id AND o.status = 'done'),
                    (SELECT COUNT(*) FROM ops o WHERE o.batch = b.id AND o.status = 'failed')
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
            "SELECT id, kind, src, dst, status, size, mtime_ns, overwrote, error
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
        for op in ops.iter().filter(|o| o.status == "done") {
            match self.undo_op(op, force, &mut report) {
                Ok(()) => {
                    self.conn.execute(
                        "UPDATE ops SET status = 'undone' WHERE id = ?1",
                        params![op.id],
                    )?;
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

    fn undo_op(&self, op: &OpRecord, force: bool, report: &mut UndoReport) -> Result<(), String> {
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
                if op.overwrote {
                    report.warnings.push(format!(
                        "{} had replaced an existing file, which cannot be restored",
                        op.to.display()
                    ));
                }
                report.restored.push((op.to.clone(), from.clone()));
            }
            OpKind::Copy | OpKind::Hardlink | OpKind::Symlink | OpKind::Create => {
                fs::remove_file(&op.to).map_err(|e| e.to_string())?;
                report.removed.push(op.to.clone());
            }
        }
        Ok(())
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
        let created = ops::apply(&op, Action::Move, false).unwrap();
        h.add_dirs(batch, &created).unwrap();
        h.finish_op(id, &op.to, false).unwrap();

        let nfo = d.path().join("out/Show/tvshow.nfo");
        fs::write(&nfo, b"<x/>").unwrap();
        let nid = h.start_op(batch, OpKind::Create, None, &nfo).unwrap();
        h.finish_op(nid, &nfo, false).unwrap();
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
        h.finish_op(id, &to, false).unwrap();
        fs::write(&to, b"changed!").unwrap();

        let report = h.undo(batch, false).unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert!(to.exists());
    }
}
