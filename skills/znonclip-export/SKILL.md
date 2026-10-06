---
name: znonclip-export
description: Token-budgeted context exporter for ZnonClip. Export files truncated to fit within a token budget for subagent prompts. Use when spawning subagents with limited context.
---

# ZnonClip Context Exporter

Exports file contents truncated to fit within a specified token budget (~4 chars/token heuristic). Ensures subagent prompts stay lean.

Every file is run through the secret scrubber (`znonclip-cli scrub`) before it is budgeted or truncated, so API keys and tokens are replaced with `[REDACTED:<type>]` in the output. A note on stderr names each file that had something redacted. The scrubber is pattern-based and will miss key shapes it does not know about, so do not export files you know hold secrets.

## Commands

```bash
# Export files within 2000-token budget
znonclip-agent export --budget 2000 file1.rs file2.rs

# Default budget is 2000 tokens
znonclip-agent export file1.rs
```

## Example Use Cases

### 1. Subagent Brief
Spawn a subagent with just enough context:
```bash
CONTEXT=$(znonclip-agent export --budget 1500 src/main.rs src/lib.rs)
# Include $CONTEXT in the subagent prompt
```

### 2. Code Review Scope
Limit review to the most relevant files:
```bash
znonclip-agent export --budget 3000 $(git diff --name-only)
```

### 3. Research Digest
Condense research for a summarizer agent:
```bash
znonclip-agent export --budget 1000 research/*.md
```

## How It Works

- Files are included in order until the budget is exhausted.
- The file that would exceed the budget is truncated to fit remaining tokens.
- Remaining files are skipped with a notice.
- Token estimate: `len(chars) / 4`.

## When to Use

- **Use** when spawning subagents — keep their context lean.
- **Use** to avoid context-window overflow in prompts.
- **Don't use** when you need full file contents — just `cat` the file.
