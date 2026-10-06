---
name: znonclip-stage
description: Patch-staging gate for ZnonClip. Syntax-check code files before they touch the filesystem. Use to validate generated code before applying patches.
---

# ZnonClip Patch-Staging Gate

Validates syntax of code files without modifying them. Prevents broken code from ever landing in the working tree. Supports Rust, Python, and JavaScript.

## Commands

```bash
# Check one file
znonclip-agent stage myfile.rs

# Check multiple files
znonclip-agent stage src/*.rs
```

## Example Use Cases

### 1. Agent-Generated Code
Before an agent writes generated code to disk:
```bash
# Agent generates code to /tmp/new-feature.rs
znonclip-agent stage /tmp/new-feature.rs
# Only if OK: cp /tmp/new-feature.rs src/
```

### 2. Patch Review
Validate a patch before applying:
```bash
# Extract patched files, stage them first
znonclip-agent stage patched-file.py && git apply patch.diff
```

### 3. CI Pre-Check
Quick syntax gate in a pipeline:
```bash
for f in $(git diff --name-only); do
  znonclip-agent stage "$f" || exit 1
done
```

## Supported Languages

| Extension | Checker |
|-----------|---------|
| `.rs` | `rustc --emit=metadata` (parse check) |
| `.py` | `python3 -m py_compile` |
| `.js`, `.mjs` | `node --check` |

Other extensions are skipped with a notice.

## When to Use

- **Use** before applying any agent-generated patch.
- **Use** in CI as a fast pre-check before full builds.
- **Don't use** as a replacement for `cargo test` or full compilation — this is syntax-only.
