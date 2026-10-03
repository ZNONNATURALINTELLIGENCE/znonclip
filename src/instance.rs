//! Single-instance guard.
//!
//! Every launch path (terminal, `--detach`, the launch-at-login agent) takes an
//! exclusive advisory lock on a file in Application Support. A second process
//! that cannot take it exits immediately, so only one menu-bar icon ever exists.
//! The kernel drops the lock when the process dies, so a crash never leaves a
//! stale lock behind.

use std::fs::{File, OpenOptions, TryLockError};

use crate::storage;

const LOCK_FILE_NAME: &str = "instance.lock";

/// Outcome of trying to become the running instance.
pub enum Acquire {
    /// We hold the lock; keep the `File` alive for the life of the process.
    Acquired(File),
    /// Another instance already holds it.
    AlreadyRunning,
    /// The lock could not be checked (no Application Support dir, IO error).
    /// Callers proceed rather than refuse to start.
    Unavailable(String),
}

pub fn acquire() -> Acquire {
    let dir = match storage::app_support_dir() {
        Ok(d) => d,
        Err(e) => return Acquire::Unavailable(e.to_string()),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Acquire::Unavailable(e.to_string());
    }
    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE_NAME))
    {
        Ok(f) => f,
        Err(e) => return Acquire::Unavailable(e.to_string()),
    };
    match file.try_lock() {
        Ok(()) => Acquire::Acquired(file),
        Err(TryLockError::WouldBlock) => Acquire::AlreadyRunning,
        Err(TryLockError::Error(e)) => Acquire::Unavailable(e.to_string()),
    }
}
