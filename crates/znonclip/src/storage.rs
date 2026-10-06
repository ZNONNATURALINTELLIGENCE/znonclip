//! SQLite persistence for clipboard history and settings.
//!
//! Database lives at:
//! `~/Library/Application Support/com.znonclip.app/znonclip.db`
//!
//! Uses WAL mode for efficient concurrent-style access (UI reads / poller writes
//! on the main thread). Schema matches `ARCHITECTURE.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use log::{debug, info, warn};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::clipboard::{ClipboardItem, ContentType};

/// Default in-memory / popover cache size (most recent items).
pub const DEFAULT_CACHE_LIMIT: usize = 100;

/// Default time-based retention for unpinned items.
#[allow(dead_code)]
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

/// Hard cap on total unpinned rows (in addition to time-based pruning).
#[allow(dead_code)]
pub const DEFAULT_MAX_ITEMS: usize = 10_000;

/// Bundle-style application support folder name: `com.znonclip.app`.
const APP_SUPPORT_QUALIFIER: &str = "com";
const APP_SUPPORT_ORG: &str = "znonclip";
const APP_SUPPORT_APP: &str = "app";
const DB_FILE_NAME: &str = "znonclip.db";

/// Errors from the storage layer.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("could not resolve Application Support directory")]
    NoProjectDirs,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// SQLite-backed clipboard history and settings store.
pub struct Storage {
    conn: Connection,
    path: PathBuf,
}

impl Storage {
    /// Filesystem path of the open database.
    #[allow(dead_code)] // Diagnostics / future settings UI
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open (or create) the database under Application Support and migrate schema.
    pub fn open_default() -> Result<Self> {
        let dir = app_support_dir()?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(DB_FILE_NAME);
        Self::open_path(&path)
    }

    /// Open a database at an explicit path (useful for tests).
    pub fn open_path(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // WAL improves read/write interleaving; safe even on a single thread.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        enable_incremental_auto_vacuum(&conn)?;

        let storage = Self {
            conn,
            path: path.to_path_buf(),
        };
        storage.migrate()?;
        info!("storage opened at {}", storage.path.display());
        Ok(storage)
    }

    /// Create tables and indexes if they do not exist.
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS clipboard_items (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                content_text         TEXT,
                content_rtf          BLOB,
                content_html         TEXT,
                content_image        BLOB,
                content_file_paths   TEXT,
                content_url          TEXT,
                source_app_bundle_id TEXT,
                content_type         TEXT NOT NULL,
                is_pinned            INTEGER NOT NULL DEFAULT 0,
                created_at           TEXT NOT NULL,
                hash                 TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_clipboard_items_created_at
                ON clipboard_items (created_at);
            CREATE INDEX IF NOT EXISTS idx_clipboard_items_is_pinned
                ON clipboard_items (is_pinned);
            CREATE INDEX IF NOT EXISTS idx_clipboard_items_hash
                ON clipboard_items (hash);

            -- Predictive-paste signal: which item was pasted into which app.
            -- Metadata only; never content. Rows vanish with their item.
            CREATE TABLE IF NOT EXISTS paste_events (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                item_id      INTEGER NOT NULL
                             REFERENCES clipboard_items(id) ON DELETE CASCADE,
                target_app   TEXT,
                content_type TEXT NOT NULL,
                pasted_at    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_paste_events_target_app
                ON paste_events (target_app);
            "#,
        )?;
        Ok(())
    }

    // ── Insert / dedup ──────────────────────────────────────────────────────

    /// Insert a new item. Returns `(id, created_at)` actually stored.
    ///
    /// Caller is responsible for consecutive-hash deduplication (see
    /// [`latest_hash`] / [`touch_latest_if_hash`]).
    pub fn insert_item(&self, item: &ClipboardItem) -> Result<(u64, String)> {
        let file_paths_json = item
            .content_file_paths
            .as_ref()
            .map(|p| serde_json::to_string(p))
            .transpose()?;

        let content_type = item.content_type.as_str();
        let is_pinned = if item.is_pinned { 1 } else { 0 };
        let created_at = if item.created_at.is_empty() {
            utc_now_iso8601()
        } else {
            item.created_at.clone()
        };

        self.conn.execute(
            r#"
            INSERT INTO clipboard_items (
                content_text, content_rtf, content_html, content_image,
                content_file_paths, content_url, source_app_bundle_id,
                content_type, is_pinned, created_at, hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                item.content_text,
                item.content_rtf,
                item.content_html,
                item.content_image,
                file_paths_json,
                item.content_url,
                item.source_app_bundle_id,
                content_type,
                is_pinned,
                created_at,
                item.hash,
            ],
        )?;

        let id = self.conn.last_insert_rowid() as u64;
        debug!("inserted clipboard item id={id} type={content_type}");
        Ok((id, created_at))
    }

    /// Hash of the most recently created item (any pin state), if any.
    #[allow(dead_code)] // Used by tests and future diagnostics
    pub fn latest_hash(&self) -> Result<Option<String>> {
        let hash: Option<String> = self
            .conn
            .query_row(
                r#"
                SELECT hash FROM clipboard_items
                ORDER BY datetime(created_at) DESC, id DESC
                LIMIT 1
                "#,
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(hash)
    }

    /// If the most recent item has `hash`, bump its `created_at` to now and
    /// return its id. Used for consecutive-duplicate dedup (no second row).
    pub fn touch_latest_if_hash(&self, hash: &str) -> Result<Option<u64>> {
        let latest: Option<(u64, String)> = self
            .conn
            .query_row(
                r#"
                SELECT id, hash FROM clipboard_items
                ORDER BY datetime(created_at) DESC, id DESC
                LIMIT 1
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let Some((id, latest_hash)) = latest else {
            return Ok(None);
        };
        if latest_hash != hash {
            return Ok(None);
        }

        let now = utc_now_iso8601();
        self.conn.execute(
            "UPDATE clipboard_items SET created_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        debug!("dedup: touched existing item id={id}");
        Ok(Some(id))
    }

    // ── Queries ─────────────────────────────────────────────────────────────

    /// Most recent items (newest first), for the in-memory popover cache.
    pub fn recent_items(&self, limit: usize) -> Result<Vec<ClipboardItem>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                id, content_text, content_rtf, content_html, content_image,
                content_file_paths, content_url, source_app_bundle_id,
                content_type, is_pinned, created_at, hash
            FROM clipboard_items
            ORDER BY is_pinned DESC, datetime(created_at) DESC, id DESC
            LIMIT ?1
            "#,
        )?;

        let rows = stmt.query_map(params![limit as i64], row_to_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Case-insensitive substring search over text / html / url / file paths.
    pub fn search_items(&self, query: &str, limit: usize) -> Result<Vec<ClipboardItem>> {
        let pattern = format!("%{}%", escape_like(query));
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                id, content_text, content_rtf, content_html, content_image,
                content_file_paths, content_url, source_app_bundle_id,
                content_type, is_pinned, created_at, hash
            FROM clipboard_items
            WHERE content_text LIKE ?1 ESCAPE '\'
               OR content_html LIKE ?1 ESCAPE '\'
               OR content_url LIKE ?1 ESCAPE '\'
               OR content_file_paths LIKE ?1 ESCAPE '\'
            ORDER BY is_pinned DESC, datetime(created_at) DESC, id DESC
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map(params![pattern, limit as i64], row_to_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Filter by content type string (`text`, `rtf`, …).
    #[allow(dead_code)] // Phase 5 type filters
    pub fn items_by_type(&self, content_type: ContentType, limit: usize) -> Result<Vec<ClipboardItem>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                id, content_text, content_rtf, content_html, content_image,
                content_file_paths, content_url, source_app_bundle_id,
                content_type, is_pinned, created_at, hash
            FROM clipboard_items
            WHERE content_type = ?1
            ORDER BY datetime(created_at) DESC, id DESC
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map(params![content_type.as_str(), limit as i64], row_to_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    // ── Pruning ─────────────────────────────────────────────────────────────

    /// Delete unpinned items older than `retention_days`, then enforce max count.
    ///
    /// **Never** deletes pinned items.
    /// Pass `retention_days = 0` to skip time-based pruning (keep by count only).
    pub fn prune(
        &self,
        retention_days: i64,
        max_items: usize,
    ) -> Result<u64> {
        let time_deleted = if retention_days > 0 {
            let cutoff = format!("-{retention_days} days");
            self.conn.execute(
                r#"
                DELETE FROM clipboard_items
                WHERE is_pinned = 0
                  AND datetime(created_at) < datetime('now', ?1)
                "#,
                params![cutoff],
            )? as u64
        } else {
            0
        };

        // Count-based prune: keep newest unpinned rows within budget after pinned.
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM clipboard_items", [], |r| r.get(0))?;
        let pinned: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM clipboard_items WHERE is_pinned = 1",
            [],
            |r| r.get(0),
        )?;

        let mut count_deleted = 0u64;
        let unpinned = total - pinned;
        let max_unpinned = max_items as i64;
        if unpinned > max_unpinned {
            let to_delete = unpinned - max_unpinned;
            // Delete oldest unpinned rows.
            count_deleted = self.conn.execute(
                r#"
                DELETE FROM clipboard_items
                WHERE id IN (
                    SELECT id FROM clipboard_items
                    WHERE is_pinned = 0
                    ORDER BY datetime(created_at) ASC, id ASC
                    LIMIT ?1
                )
                "#,
                params![to_delete],
            )? as u64;
        }

        let deleted = time_deleted + count_deleted;
        if deleted > 0 {
            // Hand freed pages (image thumbnails are the bulk) back to the OS.
            self.reclaim_free_pages()?;
            info!(
                "pruned {deleted} clipboard items (time={time_deleted}, count={count_deleted})"
            );
        }
        Ok(deleted)
    }

    /// Release free pages left by deletes. Cheap no-op when there are none.
    ///
    /// `incremental_vacuum` frees one page per statement step, so every row
    /// must be stepped; a single `execute_batch` step frees only one page.
    pub fn reclaim_free_pages(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA incremental_vacuum")?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    // ── Pin / delete ────────────────────────────────────────────────────────

    /// Toggle or set pin state. Returns the new `is_pinned` value.
    pub fn set_pinned(&self, id: u64, pinned: bool) -> Result<bool> {
        let flag = if pinned { 1 } else { 0 };
        let n = self.conn.execute(
            "UPDATE clipboard_items SET is_pinned = ?1 WHERE id = ?2",
            params![flag, id],
        )?;
        if n == 0 {
            warn!("set_pinned: no row id={id}");
        }
        Ok(pinned)
    }

    /// Delete a single history row by id (pinned or not — user-initiated).
    pub fn delete_item(&self, id: u64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM clipboard_items WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Delete many rows by id. Returns number of rows removed.
    pub fn delete_items(&self, ids: &[u64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut deleted = 0usize;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM clipboard_items WHERE id = ?1")?;
            for id in ids {
                deleted += stmt.execute(params![id])?;
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// Remove all unpinned items (pinned favorites are kept).
    pub fn clear_unpinned(&self) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM clipboard_items WHERE is_pinned = 0", [])?;
        Ok(n)
    }

    /// Remove every history row including pinned.
    #[allow(dead_code)]
    pub fn clear_all(&self) -> Result<usize> {
        let n = self.conn.execute("DELETE FROM clipboard_items", [])?;
        Ok(n)
    }

    /// Fetch one item by primary key.
    pub fn get_item(&self, id: u64) -> Result<Option<ClipboardItem>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                id, content_text, content_rtf, content_html, content_image,
                content_file_paths, content_url, source_app_bundle_id,
                content_type, is_pinned, created_at, hash
            FROM clipboard_items
            WHERE id = ?1
            "#,
        )?;
        let item = stmt
            .query_row(params![id], row_to_item)
            .optional()?;
        Ok(item)
    }

    /// Number of pinned rows (for the pin cap).
    pub fn pinned_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM clipboard_items WHERE is_pinned = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    // ── Paste events (predictive-paste signal) ──────────────────────────────

    /// Record that `item_id` was pasted into `target_app` (bundle id).
    /// Keeps only the newest [`MAX_PASTE_EVENTS`] rows.
    pub fn record_paste(
        &self,
        item_id: u64,
        target_app: Option<&str>,
        content_type: ContentType,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO paste_events (item_id, target_app, content_type, pasted_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![item_id, target_app, content_type.as_str(), utc_now_iso8601()],
        )?;
        self.conn.execute(
            r#"
            DELETE FROM paste_events WHERE id NOT IN (
                SELECT id FROM paste_events ORDER BY id DESC LIMIT ?1
            )
            "#,
            params![MAX_PASTE_EVENTS as i64],
        )?;
        Ok(())
    }

    /// Paste history for one target app: per-item and per-content-type counts.
    pub fn paste_stats(&self, target_app: Option<&str>) -> Result<PasteStats> {
        let mut stats = PasteStats::default();
        let Some(app) = target_app else {
            return Ok(stats);
        };
        let mut stmt = self.conn.prepare(
            "SELECT item_id, content_type FROM paste_events WHERE target_app = ?1",
        )?;
        let rows = stmt.query_map(params![app], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (item_id, ct) = row?;
            *stats.per_item.entry(item_id).or_insert(0) += 1;
            *stats
                .per_type
                .entry(ContentType::from_str_lossy(&ct))
                .or_insert(0) += 1;
            stats.total += 1;
        }
        Ok(stats)
    }

    // ── Settings ────────────────────────────────────────────────────────────

    #[allow(dead_code)] // Phase 7 preferences
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    #[allow(dead_code)] // Phase 7 preferences
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO settings (key, value) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![key, value],
        )?;
        Ok(())
    }
}

/// Newest paste events kept for the ranker (older rows are dropped).
pub const MAX_PASTE_EVENTS: usize = 2_000;

/// Aggregated paste history for one target app.
#[derive(Debug, Default, Clone)]
pub struct PasteStats {
    /// item id → times pasted into this app.
    pub per_item: HashMap<u64, u32>,
    /// content type → times pasted into this app.
    pub per_type: HashMap<ContentType, u32>,
    /// Total paste events for this app.
    pub total: u32,
}

pub(crate) fn app_support_dir() -> Result<PathBuf> {
    // ProjectDirs → ~/Library/Application Support/com.znonclip.app on macOS
    let dirs = ProjectDirs::from(APP_SUPPORT_QUALIFIER, APP_SUPPORT_ORG, APP_SUPPORT_APP)
        .ok_or(StorageError::NoProjectDirs)?;
    Ok(dirs.data_dir().to_path_buf())
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClipboardItem> {
    let id: u64 = row.get(0)?;
    let content_text: Option<String> = row.get(1)?;
    let content_rtf: Option<Vec<u8>> = row.get(2)?;
    let content_html: Option<String> = row.get(3)?;
    let content_image: Option<Vec<u8>> = row.get(4)?;
    let file_paths_json: Option<String> = row.get(5)?;
    let content_url: Option<String> = row.get(6)?;
    let source_app_bundle_id: Option<String> = row.get(7)?;
    let content_type_str: String = row.get(8)?;
    let is_pinned_i: i64 = row.get(9)?;
    let created_at: String = row.get(10)?;
    let hash: String = row.get(11)?;

    let content_type = ContentType::from_str_lossy(&content_type_str);
    let content_file_paths = match file_paths_json {
        Some(ref s) if !s.is_empty() => serde_json::from_str(s).ok(),
        _ => None,
    };

    let preview = ClipboardItem::make_preview(
        content_type,
        content_text.as_deref(),
        content_html.as_deref(),
        content_url.as_deref(),
        content_file_paths.as_deref(),
        content_image.is_some(),
        content_rtf.is_some(),
    );

    Ok(ClipboardItem {
        id,
        content_type,
        content_text,
        content_rtf,
        content_html,
        content_image,
        content_file_paths,
        content_url,
        source_app_bundle_id,
        is_pinned: is_pinned_i != 0,
        created_at,
        hash,
        preview,
    })
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// UTC timestamp as `YYYY-MM-DDTHH:MM:SS.mmmZ` using SQLite's clock for consistency.
fn utc_now_iso8601() -> String {
    // Prefer process clock formatted simply; SQLite also accepts this form for datetime().
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Use a lightweight formatter via SQLite-compatible subset if needed;
    // store as epoch-ms ISO-ish: still sortable. Better: format properly.
    format_unix_ms_iso8601(ms as u64)
}

fn format_unix_ms_iso8601(ms: u64) -> String {
    // Civil date/time from Unix millis (UTC), no external chrono dependency.
    let secs = (ms / 1000) as i64;
    let millis = (ms % 1000) as u32;
    let (y, mo, d, h, mi, s) = unix_secs_to_utc_parts(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// Convert Unix seconds to (year, month, day, hour, min, sec) in UTC.
fn unix_secs_to_utc_parts(mut secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    // Algorithm based on civil_from_days (Howard Hinnant).
    if secs < 0 {
        // ZnonClip targets modern macOS; negative timestamps are unexpected.
        warn!("negative unix timestamp; clamping to 0");
        secs = 0;
    }
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400) as u32;
    let hour = day_secs / 3600;
    let min = (day_secs % 3600) / 60;
    let sec = day_secs % 60;

    // days since Unix epoch (1970-01-01) → civil date
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hour, min, sec)
}

/// `auto_vacuum` mode 2 (INCREMENTAL). Without it SQLite never shrinks the file:
/// pruned thumbnails leave free pages behind forever.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// Switch the database to incremental auto-vacuum. A database created before
/// this setting needs one full `VACUUM` to restructure; that runs once, on the
/// first open after upgrade, and never again.
fn enable_incremental_auto_vacuum(conn: &Connection) -> Result<()> {
    let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
    if mode == AUTO_VACUUM_INCREMENTAL {
        return Ok(());
    }
    conn.pragma_update(None, "auto_vacuum", AUTO_VACUUM_INCREMENTAL)?;
    conn.execute_batch("VACUUM;")?;
    info!("storage: switched auto_vacuum {mode} -> incremental");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardItem;

    fn sample_item(text: &str, hash: &str) -> ClipboardItem {
        ClipboardItem {
            id: 0,
            content_type: ContentType::Text,
            content_text: Some(text.to_string()),
            content_rtf: None,
            content_html: None,
            content_image: None,
            content_file_paths: None,
            content_url: None,
            source_app_bundle_id: None,
            is_pinned: false,
            created_at: utc_now_iso8601(),
            hash: hash.to_string(),
            preview: text.to_string(),
        }
    }

    #[test]
    fn insert_query_dedup_and_prune() {
        let dir = std::env::temp_dir().join(format!("znonclip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.db");
        let storage = Storage::open_path(&db).unwrap();

        let (id1, _) = storage.insert_item(&sample_item("hello", "h1")).unwrap();
        assert!(id1 > 0);
        assert_eq!(storage.latest_hash().unwrap().as_deref(), Some("h1"));

        // Consecutive same hash → touch, no new row
        assert_eq!(storage.touch_latest_if_hash("h1").unwrap(), Some(id1));
        assert_eq!(storage.recent_items(10).unwrap().len(), 1);

        let (id2, _) = storage.insert_item(&sample_item("world", "h2")).unwrap();
        assert_ne!(id1, id2);
        assert_eq!(storage.recent_items(10).unwrap().len(), 2);

        let found = storage.search_items("hel", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].content_text.as_deref(), Some("hello"));

        // Pin + delete
        storage.set_pinned(id1, true).unwrap();
        let pinned = storage.get_item(id1).unwrap().unwrap();
        assert!(pinned.is_pinned);
        assert!(storage.delete_item(id2).unwrap());
        assert!(storage.get_item(id2).unwrap().is_none());
        // Pinned row still present
        assert!(storage.get_item(id1).unwrap().is_some());
        assert_eq!(storage.pinned_count().unwrap(), 1);

        // Paste events aggregate per app and cascade-delete with the item
        storage.record_paste(id1, Some("com.example.editor"), ContentType::Text).unwrap();
        storage.record_paste(id1, Some("com.example.editor"), ContentType::Text).unwrap();
        storage.record_paste(id1, Some("com.example.term"), ContentType::Text).unwrap();
        let st = storage.paste_stats(Some("com.example.editor")).unwrap();
        assert_eq!(st.total, 2);
        assert_eq!(st.per_item.get(&id1), Some(&2));
        assert_eq!(st.per_type.get(&ContentType::Text), Some(&2));
        assert_eq!(storage.paste_stats(None).unwrap().total, 0);
        assert!(storage.delete_item(id1).unwrap());
        assert_eq!(storage.paste_stats(Some("com.example.editor")).unwrap().total, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_db_gains_incremental_auto_vacuum_and_shrinks() {
        let dir = std::env::temp_dir().join(format!("znonclip-vac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("legacy.db");
        {
            // A pre-upgrade database: default auto_vacuum = NONE.
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
            let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap();
            assert_eq!(mode, 0);
        }
        let storage = Storage::open_path(&db).unwrap();
        let mode: i64 = storage
            .conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, AUTO_VACUUM_INCREMENTAL);

        // ~4 MB of image blobs, then prune them all: page count must drop.
        for n in 0..16 {
            let mut item = sample_item(&format!("img{n}"), &format!("vh{n}"));
            item.content_type = ContentType::Image;
            item.content_image = Some(vec![n as u8; 256 * 1024]);
            storage.insert_item(&item).unwrap();
        }
        let pages = |s: &Storage| -> i64 {
            s.conn.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap()
        };
        let before = pages(&storage);
        storage.prune(0, 0).unwrap();
        let after = pages(&storage);
        assert!(after < before / 4, "pages before={before} after={after}");

        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
