//! On-disk cache for provider responses (`~/.cache/mrn/cache.db`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

pub struct Cache {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Cache {
    pub fn open(path: &Path) -> Result<Cache> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening cache {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cache (
                key     TEXT PRIMARY KEY,
                value   TEXT NOT NULL,
                expires INTEGER NOT NULL
            );",
        )?;
        // Opportunistic cleanup.
        conn.execute("DELETE FROM cache WHERE expires <= ?1", params![now()])?;
        Ok(Cache {
            conn: Mutex::new(conn),
        })
    }

    pub fn default_path() -> Result<PathBuf> {
        Ok(crate::config::dirs()?.cache.join("cache.db"))
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT value FROM cache WHERE key = ?1 AND expires > ?2",
            params![key, now()],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn put(&self, key: &str, value: &str, ttl: Duration) {
        let Ok(conn) = self.conn.lock() else { return };
        let expires = now() + ttl.as_secs() as i64;
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO cache (key, value, expires) VALUES (?1, ?2, ?3)",
            params![key, value, expires],
        ) {
            tracing::warn!("cache write failed: {e}");
        }
    }

    /// Delete every entry. Returns how many were removed.
    pub fn clear(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("cache lock poisoned"))?;
        Ok(conn.execute("DELETE FROM cache", [])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_expiry() {
        let d = tempfile::tempdir().unwrap();
        let c = Cache::open(&d.path().join("c.db")).unwrap();
        c.put("a", "1", Duration::from_secs(60));
        c.put("b", "2", Duration::from_secs(0));
        assert_eq!(c.get("a").as_deref(), Some("1"));
        assert_eq!(c.get("b"), None);
        assert_eq!(c.clear().unwrap(), 2);
    }
}
