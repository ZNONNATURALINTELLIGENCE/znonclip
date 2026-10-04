# ZnonClip

A small, native macOS menu-bar clipboard manager. It keeps your **last 20 copies**
and up to **20 pins**, opens with a hotkey or a click on the menu-bar icon (left,
right or two-finger), and **pre-highlights the item you most likely want to
paste**, so the usual case is a single key: Enter.

It is written in Rust with native AppKit: no Electron, no webview. Everything
stays on your Mac.

> ZnonClip is a fork of **[ClipPin](https://github.com/kushwahramkumar2003/clippin)**
> by the ClipPin contributors (MIT). The menu-bar shell, privacy filter, SQLite
> store, hotkey and auto-paste come from ClipPin. ZnonClip adds the
> predictive highlight, keyboard paste, right-click and two-finger-click opening,
> source and target app tracking, and the 20/20 working set.

**Requires:** macOS 13+, [Rust](https://rustup.rs/), Xcode Command Line Tools.

---

## Install

```bash
git clone https://github.com/ZNONNATURALINTELLIGENCE/znonclip.git
cd znonclip
cargo install --path . --locked
```

### Run

```bash
znonclip --detach     # background; the terminal can close
znonclip              # foreground, with logs (useful for debugging)
```

The app lives in the **menu bar** and has no Dock icon.

| | |
|--|--|
| **Stop** | `pkill -x znonclip` |
| **Logs** | `~/Library/Logs/ZnonClip/znonclip.log` (detached mode) |
| **Help** | `znonclip --help` |

### Uninstall

```bash
cargo uninstall znonclip
rm -rf ~/Library/Application\ Support/com.znonclip.app      # history + settings
rm -f ~/Library/LaunchAgents/com.znonclip.app.plist          # if you used launch at login
```

---

## Use

| Action | How |
|---|---|
| Open | **⌃⌘V**, or click the menu-bar icon (left, right or two-finger) |
| Paste the suggested item | **Enter** |
| Pick another | **↑ / ↓**, then Enter, or click a row |
| Close | **Esc** (focus returns to your app) |
| Search | Type; the suggestion is switched off while you search |
| Pin / unpin / delete | Right-click a row |
| Settings | Gear icon |

### What "suggested" means

When the menu opens, a local ranker scores your recent copies and pins and
highlights the best guess with ✨. Hover over the row to see why, for example
*"often pasted into this app"* or *"most recent copy"*. The suggestion is only a
highlight. The list order never changes, and nothing is pasted until you press
Enter or click.

The signals it uses: how recent each copy is, whether it is pinned, which app
you are pasting into, what you have pasted into that app before, the content
type, and whether the item is already on the clipboard. It records **which**
item you pasted **where**, never the content. Details and the plan for adding a
small local model are in [docs/architecture.md](docs/architecture.md).

---

## Privacy

- Copies from password managers, and anything else that marks the pasteboard
  `org.nspasteboard.ConcealedType` or `TransientType`, are skipped **before** the
  data is read. They never reach memory, disk or the ranker.
- History, pins and paste statistics stay in a local SQLite file. There is no
  network code.

## Permissions

| Permission | Why |
|---|---|
| **Accessibility** | To paste for you (synthesizes ⌘V). Without it, Enter copies and you press ⌘V yourself. |
| **Login item / LaunchAgent** | Optional launch at login |

Grant Accessibility under **System Settings → Privacy & Security → Accessibility**.

## Memory

Idle memory is measured, not estimated. To check it on your own Mac:

```bash
scripts/measure-rss.sh
```

Measured on an Apple Silicon Mac (macOS 26), 20 items in history, floater built
and idle: **17 MB phys_footprint** (the number Activity Monitor shows), 12 MB RSS.

## License

MIT. See [LICENSE](LICENSE). Original work © the ClipPin contributors;
modifications © the ZnonClip contributors.
