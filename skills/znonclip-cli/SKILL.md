---
name: znonclip-cli
description: Terminal interface for ZnonClip clipboard manager. Pin sprint rules, stash research, query history, and scrub secrets — all from the command line.
---

# ZnonClip CLI

Terminal interface to the shared ZnonClip store. All crates use the same SQLite database.

## Commands

```bash
# Pin text as an active rule (pinned items sort first)
znonclip-cli pin "Never force-push to main"

# Stash text (auto-scrubs secrets first)
znonclip-cli stash '{"findings": [...]}'

# Show recent clips (default 10, pinned first)
znonclip-cli recent
znonclip-cli recent 20

# Scrub secrets from text
znonclip-cli scrub "Key: AKIA..."
```

## Example Use Cases

### 1. Sprint Rules (50 tokens vs 600-line AGENTS.md)
```bash
# At sprint start, pin the active rules:
znonclip-cli pin "RULE: Every commit references an issue"
znonclip-cli pin "RULE: Shared config files have a single owner"
znonclip-cli pin "RULE: Never commit secrets"

# Later, recall them cheaply:
znonclip-cli recent 5
```

### 2. Research Stash
Stash raw JSON mid-audit without polluting git:
```bash
znonclip-cli stash "$(curl -s https://api.example.com/data)"
# Secrets auto-scrubbed on stash
```

### 3. Attestation Staging
Stage timestamps before on-chain anchoring:
```bash
znonclip-cli stash "2026-10-06T07:30:00Z anchor sha256:abc123"
```

### 4. Quick Lookup
Find that command you copied yesterday:
```bash
znonclip-cli recent 50 | grep "kubectl"
```

## Storage

- Location: `~/.local/share/ZnonClip/clips.db` (via `directories` crate)
- Pinned items sort before unpinned in `recent`
- Auto-vacuum keeps the DB file small
