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
        // Enable auto-vacuum to keep the DB file small
        conn.execute("PRAGMA auto_vacuum = INCREMENTAL", [])?;
        Ok(Self { conn })
    }

    /// Open the default store location.
    pub fn open_default() -> Result<Self> {
        let dir = directories::ProjectDirs::from("com", "znonclip", "ZnonClip")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)?;
        Self::open(dir.join("clips.db"))
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
