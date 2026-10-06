# ZnonClip

**The zero-latency, local-first clipboard manager that predicts your next paste.**

ZnonClip is a Cargo workspace with a native macOS app *and* agent tools that fill the structural hole between heavy persistent memory and raw shell pipes.

## Crates

| Crate | What it is |
|-------|-----------|
| `znonclip` | Native macOS menu-bar clipboard manager (Rust + AppKit, no Electron). 15–30MB idle RSS. Predictive highlight, pin/unpin, auto-paste, Carbon hotkeys. |
| `znonclip-core` | Shared library: SQLite storage (with auto-vacuum), privacy filters, **pre-prompt secret scrubbing**. |
| `znonclip-cli` | Terminal interface: `pin`, `stash`, `recent`, `scrub`. Pin sprint rules, stash research JSON. |
| `znonclip-agent` | Agent tools: **IPC bus**, **patch-staging gate**, **token-budgeted context exporter**. |

## Agent Tools

### Pre-prompt Secret Scrubbing
Pipe text through the scrubber to strip API keys, tokens, and mnemonics *before* they enter an LLM's context window:
```bash
znonclip-cli scrub "My key is AKIA..."
# → "My key is [REDACTED:aws-key]"
```

### Cross-Agent IPC Bus
Write a diff to a shared slot; another agent reads it in microseconds instead of routing through git:
```bash
znonclip-agent ipc write sprint-rules "Rule 1: ..."
znonclip-agent ipc read sprint-rules
```

### Patch-Staging Gate
Syntax-check code before it touches the filesystem:
```bash
znonclip-agent stage myfile.rs
```

### Context Exporter
Export files truncated to fit within a token budget for subagent prompts:
```bash
znonclip-agent export --budget 2000 file1.rs file2.rs
```

## Mac App

A small, native macOS menu-bar clipboard manager. It keeps your **last 20 copies** and up to **20 pins**, opens with a hotkey or a click on the menu-bar icon, and **pre-highlights the item you most likely want to paste**.

Written in Rust with native AppKit: no Electron, no webview. Everything stays on your Mac.

> ZnonClip is a fork of **[ClipPin](https://github.com/kushwahramkumar2003/clippin)** by the ClipPin contributors (MIT). The menu-bar shell, privacy filter, SQLite store, hotkey and auto-paste come from ClipPin. ZnonClip adds the predictive highlight, keyboard paste, right-click and two-finger-click opening, source and target app tracking, and the 20/20 working set.

**Requires:** macOS 13+, [Rust](https://rustup.rs/), Xcode Command Line Tools.

## Building

```bash
# Agent tools and CLI (any platform)
cargo build -p znonclip-core -p znonclip-cli -p znonclip-agent

# Mac app (macOS only)
cargo build -p znonclip
```

## License

MIT. See [LICENSE](LICENSE).
