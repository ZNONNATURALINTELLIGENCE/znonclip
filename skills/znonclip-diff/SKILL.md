---
name: znonclip-diff
description: Safe slot/file diff viewer for ZnonClip. Compare IPC slots against files or files against files with unified diff output, without touching git. Use for auditor review of proposed patches.
---

# ZnonClip Diff Viewer

Compares IPC slots against disk files (or files against files) and outputs unified diff syntax. Designed for auditor agents to review proposed changes before application, without requiring git.

## Commands

```bash
# Diff an IPC slot against a file (most common: review proposed patch)
znonclip-agent diff slot:proposed-patch src/main.rs

# Diff two files
znonclip-agent diff old_version.rs new_version.rs

# Diff two slots
znonclip-agent diff slot:baseline slot:candidate
```

## Output Format

Unified diff with 3 lines of context:
```
--- slot:proposed-patch
+++ src/main.rs
@@ -1,3 +1,4 @@
 line1
-line2
+line2 MODIFIED
 line3
+line4 new
```

Footer shows line counts and change stats.

## Example Use Cases

### 1. Auditor Review
An agent proposes a patch via IPC slot; an auditor reviews before applying:
```bash
# Proposer writes patch to slot
znonclip-agent ipc write proposed-patch "$(cat /tmp/patch.rs)"

# Auditor reviews the diff
znonclip-agent diff slot:proposed-patch src/main.rs
# If approved: znonclip-agent ipc read proposed-patch > src/main.rs
```

### 2. Pre-Apply Verification
Verify what changed before overwriting:
```bash
# See exactly what would change
znonclip-agent diff slot:new-config config.toml
```

### 3. Slot Comparison
Compare two versions stored in slots:
```bash
znonclip-agent diff slot:v1 slot:v2
```

## When to Use

- **Use** when an auditor needs to review a proposed change before it lands.
- **Use** to verify slot content matches expectations before applying.
- **Don't use** as a replacement for `git diff` when you need history-aware diffs — this is for slot/file comparison only.
