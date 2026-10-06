---
name: znonclip-ipc
description: Cross-agent local IPC bus for ZnonClip. Write data to a shared slot; another agent reads it in microseconds instead of routing through git. Use for handoffs, shared state, and fast agent-to-agent communication on the same machine.
---

# ZnonClip IPC Bus

The IPC bus provides named shared slots for cross-agent communication on the same machine. Writes are atomic (temp file + rename). Slot names are sanitized (alphanumeric, `-`, `_` only).

## Commands

```bash
# Write to a slot
znonclip-agent ipc write <slot> <content>

# Read from a slot
znonclip-agent ipc read <slot>

# List all slots
znonclip-agent ipc list
```

## Example Use Cases

### 1. Sprint Rules Handoff
Pin the 3–5 active rules for a sprint so a query costs 50 tokens instead of re-reading 600 lines of AGENTS.md:
```bash
# Agent A (at sprint start):
znonclip-agent ipc write sprint-rules "1. Never force-push. 2. Every commit references an issue. 3. Shared config files have a single owner."

# Agent B (needs the rules):
RULES=$(znonclip-agent ipc read sprint-rules)
```

### 2. Research Handoff
Stash raw research JSON mid-audit without polluting git:
```bash
# Agent A (researcher):
znonclip-agent ipc write audit-findings '{"critical": 3, "high": 12}'

# Agent B (reviewer):
znonclip-agent ipc read audit-findings | jq .critical
```

### 3. Attestation Timestamp Staging
Stage attestation timestamps before anchoring:
```bash
znonclip-agent ipc write attest-queue "2026-10-06T07:30:00Z:anchor:sha256:abc123"
```

### 4. Build Status Signal
Signal build completion to a waiting agent:
```bash
# Builder:
znonclip-agent ipc write build-status "OK: 5a060e6"

# Watcher (polls):
while [ "$(znonclip-agent ipc read build-status)" != "OK: 5a060e6" ]; do sleep 5; done
```

## When to Use

- **Use** when two agents on the same machine need to share data fast (<1ms).
- **Use** for ephemeral state that shouldn't go in git (build status, temp findings).
- **Don't use** for durable history — use `znonclip-cli stash` or git for that.
- **Don't use** across machines — slots are local-only.

## Slot Naming

- Use descriptive names: `sprint-rules`, `audit-findings`, `build-status`
- Prefix with project for isolation: `myproject-rules`
- Slots persist until overwritten; clean up with `rm` on the slot file if needed.
