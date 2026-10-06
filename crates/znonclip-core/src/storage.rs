//! SQLite-backed clip storage shared across all ZnonClip crates.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::PathBuf;

/// A stored clipboard item.
#[derive(Debug, Clone)]
pub struct ClipItem {
    pub id: i64,
    pub content: String,
    pub content_type: String,
    pub pinned: bool,
    pub created_at: i64,
}

/// Shared clip store backed by SQLite.
pub struct ClipStore {
    conn: Connection,
}

impl ClipStore {
    /// Open (or create) the store at the given path.
    pub fn open(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(path)?;
        enable_incremental_auto_vacuum(&conn)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS clips (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                content_type TEXT NOT NULL DEFAULT 'text',
                pinned INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(Self { conn })
    }

    /// Open the default store location.
    pub fn open_default() -> Result<Self> {
        let dir = directories::ProjectDirs::from("com", "znonclip", "ZnonClip")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)?;
        let db = dir.join("clips.db");
        let store = Self::open(db.clone())?;
        restrict_permissions(&dir, &db);
        Ok(store)
    }

    /// Insert a new clip, returning its ID.
    pub fn insert(&self, content: &str, content_type: &str) -> Result<i64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        self.conn.execute(
            "INSERT INTO clips (content, content_type, created_at) VALUES (?1, ?2, ?3)",
            params![content, content_type, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Pin or unpin a clip.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE clips SET pinned = ?1 WHERE id = ?2",
            params![pinned as i32, id],
        )?;
        Ok(())
    }

    /// How many clips are pinned.
    pub fn pinned_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM clips WHERE pinned = 1", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Get recent clips, pinned first.
    pub fn recent(&self, limit: usize) -> Result<Vec<ClipItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, content_type, pinned, created_at FROM clips
             ORDER BY pinned DESC, created_at DESC LIMIT ?1",
        )?;
        let items = stmt
            .query_map(params![limit as i64], |row| {
                Ok(ClipItem {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    content_type: row.get(2)?,
                    pinned: row.get::<_, i32>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    }
}

/// Clipboard history can hold anything you copied: keep it owner-only.
/// SQLite gives its -wal/-shm files the database file's mode.
pub fn restrict_permissions(dir: &std::path::Path, db: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        let p = std::path::PathBuf::from(p);
        if p.exists() {
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// `auto_vacuum` mode 2 (INCREMENTAL), so deleted rows can be given back to the OS.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// Switch the database to incremental auto-vacuum. SQLite only honours this
/// before the first table exists, or after a full `VACUUM`; a store created
/// without it gets that one `VACUUM` on its next open, and never again.
fn enable_incremental_auto_vacuum(conn: &Connection) -> Result<()> {
    let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
    if mode == AUTO_VACUUM_INCREMENTAL {
        return Ok(());
    }
    conn.pragma_update(None, "auto_vacuum", AUTO_VACUUM_INCREMENTAL)?;
    conn.execute_batch("VACUUM;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("znonclip-core-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("clips.db")
    }

    fn auto_vacuum_mode(path: &PathBuf) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn new_store_uses_incremental_auto_vacuum() {
        let path = temp_db("new");
        ClipStore::open(path.clone()).unwrap();
        assert_eq!(auto_vacuum_mode(&path), AUTO_VACUUM_INCREMENTAL);
    }

    #[test]
    fn legacy_store_is_migrated_and_keeps_rows() {
        let path = temp_db("legacy");
        {
            // A store as older builds created it: table first, auto_vacuum off.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE clips (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    content TEXT NOT NULL,
                    content_type TEXT NOT NULL DEFAULT 'text',
                    pinned INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL
                );
                INSERT INTO clips (content, created_at) VALUES ('kept', 1);",
            )
            .unwrap();
        }
        assert_eq!(auto_vacuum_mode(&path), 0);

        let store = ClipStore::open(path.clone()).unwrap();
        assert_eq!(auto_vacuum_mode(&path), AUTO_VACUUM_INCREMENTAL);
        let items = store.recent(10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].content, "kept");
    }
}
