---
name: znonclip-snap
description: Ephemeral snapshot & rollback for ZnonClip. Snapshot targeted files before risky refactors into a local SQLite stash. Roll back without touching git (which would wipe unrelated uncommitted work).
---

# ZnonClip Snapshot & Rollback

Creates lightweight snapshots of files before risky operations. If tests fail or the refactor goes wrong, roll back to the snapshot without using `git checkout -- .` (which is destructive to unrelated uncommitted work).

Snapshots are stored in a local SQLite database (`~/.local/share/znonclip/agent/snapshots.db` on Linux, `~/Library/Application Support/com.znonclip.ZnonClip/agent/snapshots.db` on Mac).

## Commands

```bash
# Snapshot files before a refactor
znonclip-agent snap create pre-refactor src/main.rs src/lib.rs

# ... do the refactor, run tests ...

# If tests fail: roll back
znonclip-agent snap rollback pre-refactor

# List all snapshots
znonclip-agent snap list

# Delete a snapshot when done
znonclip-agent snap delete pre-refactor
```

## Example Use Cases

### 1. Risky Refactor
```bash
# Snapshot before the refactor
znonclip-agent snap create pre-refactor src/main.rs src/lib.rs src/utils.rs

# Do the refactor...
# Run tests...
cargo test

# If tests fail:
znonclip-agent snap rollback pre-refactor
# Files are restored, unrelated uncommitted work is untouched

# If tests pass:
znonclip-agent snap delete pre-refactor
```

### 2. Multi-File Experiment
```bash
# Try an experimental change across 4 files
znonclip-agent snap create experiment-1 a.rs b.rs c.rs d.rs
# ... make changes ...
# Didn't work? Roll back
znonclip-agent snap rollback experiment-1
```

### 3. Agent Safety Net
An agent can snapshot before any bulk edit:
```bash
znonclip-agent snap create agent-backup-$(date +%s) $(git diff --name-only)
```

## When to Use

- **Use** before any multi-file refactor or experimental change.
- **Use** when `git checkout -- .` would be too destructive (unrelated uncommitted work exists).
- **Don't use** as a replacement for git commits — snapshots are ephemeral, not versioned history.
- **Don't use** for long-term backups — snapshots are for short-lived rollback during active work.

## Storage

Snapshots are stored as binary blobs in SQLite. They persist across sessions but are not synced or versioned. Delete them when the work is complete to save disk space.
