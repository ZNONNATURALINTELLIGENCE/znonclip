//! Ephemeral snapshot & rollback.
//!
//! Snapshot targeted files before a risky refactor into a
//! local SQLite stash. If tests fail, roll back without touching git
//! (which would wipe unrelated uncommitted work).
//!
//! Usage:
//!   znonclip-agent snap create <name> <file>...  - snapshot files
//!   znonclip-agent snap rollback <name>          - restore files
//!   znonclip-agent snap list                     - list snapshots
//!   znonclip-agent snap delete <name>            - delete a snapshot

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn db_path() -> Result<PathBuf> {
    let dir = crate::agent_dir()?;
    Ok(dir.join("snapshots.db"))
}

fn open_db() -> Result<Connection> {
    let path = db_path()?;
    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open snapshot DB at {}", path.display()))?;
    // Snapshots are plaintext copies of the files: owner-only, like the slots.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    conn.execute(
        "CREATE TABLE IF NOT EXISTS snapshots (
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            content BLOB NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (name, path)
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_snapshots_name ON snapshots(name)",
        [],
    )?;
    Ok(conn)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("Usage: znonclip-agent snap <create|rollback|list|delete> [...]");
        eprintln!("  snap create <name> <file>...  - Snapshot files into a named stash");
        eprintln!("  snap rollback <name>          - Restore files from a snapshot");
        eprintln!("  snap list                     - List snapshots");
        eprintln!("  snap delete <name>            - Delete a snapshot");
        std::process::exit(1);
    }

    match args[0].as_str() {
        "create" => {
            if args.len() < 3 {
                eprintln!("Usage: znonclip-agent snap create <name> <file>...");
                std::process::exit(1);
            }
            let name = &args[1];
            let conn = open_db()?;
            let ts = now_secs();
            let mut count = 0;
            for file in &args[2..] {
                let content = std::fs::read(file)
                    .with_context(|| format!("Failed to read {file}"))?;
                // Store the absolute path, so rollback restores this file even
                // when it runs from a different working directory.
                let abs = std::fs::canonicalize(file)
                    .with_context(|| format!("Failed to resolve {file}"))?;
                conn.execute(
                    "INSERT OR REPLACE INTO snapshots (name, path, content, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![name, abs.to_string_lossy(), content, ts],
                )?;
                count += 1;
            }
            println!("Snapshot '{name}': saved {count} file(s).");
        }
        "rollback" => {
            if args.len() < 2 {
                eprintln!("Usage: znonclip-agent snap rollback <name>");
                std::process::exit(1);
            }
            let name = &args[1];
            let conn = open_db()?;
            let mut stmt = conn.prepare(
                "SELECT path, content FROM snapshots WHERE name = ?1",
            )?;
            let rows: Vec<(String, Vec<u8>)> = stmt
                .query_map(params![name], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<Result<_, _>>()?;

            if rows.is_empty() {
                anyhow::bail!("No snapshot found with name '{name}'");
            }
            // Older snapshots stored paths as typed. A relative path would be
            // restored against the current directory, maybe over another file.
            if let Some((path, _)) = rows.iter().find(|(p, _)| !std::path::Path::new(p).is_absolute()) {
                anyhow::bail!(
                    "Snapshot '{name}' has a relative path ({path}) from an older version; \
                     nothing restored. Delete it and snapshot again."
                );
            }

            let mut count = 0;
            for (path, content) in rows {
                // Write atomically via temp file + rename
                let tmp = format!("{path}.znonclip-snap-tmp");
                std::fs::write(&tmp, &content)
                    .with_context(|| format!("Failed to write {path}"))?;
                std::fs::rename(&tmp, &path)
                    .with_context(|| format!("Failed to restore {path}"))?;
                count += 1;
            }
            println!("Rolled back snapshot '{name}': restored {count} file(s).");
        }
        "list" => {
            let conn = open_db()?;
            let mut stmt = conn.prepare(
                "SELECT name, COUNT(*), MAX(created_at) FROM snapshots
                 GROUP BY name ORDER BY MAX(created_at) DESC",
            )?;
            let rows: Vec<(String, i64, i64)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<Result<_, _>>()?;

            if rows.is_empty() {
                println!("No snapshots.");
            } else {
                for (name, file_count, ts) in rows {
                    println!("{name}: {file_count} file(s), created {ts}");
                }
            }
        }
        "delete" => {
            if args.len() < 2 {
                eprintln!("Usage: znonclip-agent snap delete <name>");
                std::process::exit(1);
            }
            let name = &args[1];
            let conn = open_db()?;
            let deleted = conn.execute(
                "DELETE FROM snapshots WHERE name = ?1",
                params![name],
            )?;
            if deleted == 0 {
                anyhow::bail!("No snapshot found with name '{name}'");
            }
            println!("Deleted snapshot '{name}' ({deleted} file entries).");
        }
        _ => {
            eprintln!("Unknown snap subcommand: {}", args[0]);
            std::process::exit(1);
        }
    }
    Ok(())
}
