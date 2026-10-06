---
name: znonclip-scrub
description: Pre-prompt secret scrubbing for ZnonClip. Strip API keys, tokens, and mnemonics from text BEFORE it enters an LLM's context window. Use before sending scraped text, logs, or code to any model.
---

# ZnonClip Secret Scrubber

Pipes text through pattern matchers to redact credentials before they can leak into a model's context window. Replaces secrets with `[REDACTED:<type>]`.

## Commands

```bash
# Scrub text from arguments
znonclip-cli scrub "My key is AKIAIOSFODNN7EXAMPLE"

# Scrub a file before sending to an LLM
znonclip-cli scrub "$(cat scraped-page.txt)"

# Scrub and stash (scrub happens automatically on stash)
znonclip-cli stash "$(cat research.json)"
```

## What Gets Redacted

| Pattern | Example | Redacted As |
|---------|---------|-------------|
| AWS keys | `AKIAIOSFODNN7EXAMPLE` | `[REDACTED:aws-key]` |
| GitHub tokens | `ghp_...`, `gho_...`, `github_pat_...` | `[REDACTED:github-token]` |
| OpenAI keys | `sk-...` | `[REDACTED:openai-key]` |
| Bearer tokens | `Bearer eyJ...` | `Bearer [REDACTED:bearer-token]` |
| PEM private keys | `-----BEGIN PRIVATE KEY-----` | `[REDACTED:private-key]` |
| Mnemonics | 12/24-word phrases near "seed"/"mnemonic" | `[REDACTED:mnemonic]` |

## Example Use Cases

### 1. Scraped Web Content
Before sending scraped documentation to an LLM for summarization:
```bash
RAW=$(curl -s https://example.com/docs)
CLEAN=$(znonclip-cli scrub "$RAW")
# Now safe to send $CLEAN to the model
```

### 2. Log Files
Scrub logs before asking an LLM to diagnose:
```bash
znonclip-cli scrub "$(cat /var/log/app.log)" > clean-log.txt
```

### 3. Code Review
Scrub a diff before posting for review:
```bash
git diff | znonclip-cli scrub "$(cat)" 
```

### 4. Research JSON
Stashing automatically scrubs:
```bash
znonclip-cli stash '{"api_key": "sk-abc123", "data": "..."}'
# Stored as: '{"api_key": "[REDACTED:openai-key]", "data": "..."}'
```

## When to Use

- **Always** before sending untrusted or scraped text to an LLM.
- **Always** before logging text that might contain credentials.
- **Use** in pipelines: `scrape | scrub | summarize`.

## Limitations

- Pattern-based; sophisticated obfuscation may bypass it.
- Mnemonic detection is heuristic (requires sensitive context words).
- When in doubt, manually review — the scrubber is a safety net, not a guarantee.
