//! znonclip-cli: terminal interface for ZnonClip.
//!
//! Pin rules, stash notes, list recent clips, and scrub secrets from text.
//! Text comes from the arguments or, when there are none, from stdin, so a
//! secret never has to appear in `ps` or shell history:
//! `pbpaste | znonclip-cli scrub`.

use std::io::{IsTerminal, Read};

use anyhow::{bail, Result};
use znonclip_core::{scrub_secrets, ClipStore};

/// Same cap as the macOS app.
const MAX_PINNED: usize = 20;

fn usage() -> ! {
    eprintln!("Usage: znonclip-cli <pin|stash|recent|scrub> [...]");
    eprintln!("  pin [text]     Pin text as an active rule (scrubbed; max {MAX_PINNED} pins)");
    eprintln!("  stash [text]   Stash text, e.g. research JSON (scrubbed)");
    eprintln!("  recent [n]     Show recent clips, pinned first (default 10)");
    eprintln!("  scrub [text]   Print text with known secrets redacted");
    eprintln!("Without [text], the text is read from stdin.");
    std::process::exit(1);
}

/// The command's text: its arguments, else stdin.
fn input(rest: &[String]) -> Result<String> {
    if !rest.is_empty() {
        return Ok(rest.join(" "));
    }
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("no text given: pass it as arguments or pipe it on stdin");
    }
    let mut s = String::new();
    stdin.read_to_string(&mut s)?;
    Ok(s.trim_end_matches('\n').to_string())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(cmd) = args.get(1) else { usage() };
    let rest = &args[2..];

    match cmd.as_str() {
        "scrub" => println!("{}", scrub_secrets(&input(rest)?)),
        "pin" => {
            let store = ClipStore::open_default()?;
            if store.pinned_count()? >= MAX_PINNED {
                bail!("{MAX_PINNED} pins max: unpin one first");
            }
            let text = scrub_secrets(&input(rest)?);
            let id = store.insert(&text, "text")?;
            store.set_pinned(id, true)?;
            println!("Pinned clip #{id}");
        }
        "stash" => {
            let store = ClipStore::open_default()?;
            let id = store.insert(&scrub_secrets(&input(rest)?), "stash")?;
            println!("Stashed clip #{id}");
        }
        "recent" => {
            let store = ClipStore::open_default()?;
            let n: usize = rest.first().and_then(|s| s.parse().ok()).unwrap_or(10);
            for item in store.recent(n)? {
                let pin = if item.pinned { "[PIN] " } else { "" };
                // Older rows may predate scrubbing: scrub on the way out too.
                let preview: String = scrub_secrets(&item.content).chars().take(80).collect();
                println!("#{id} {pin}{preview}", id = item.id);
            }
        }
        _ => {
            eprintln!("Unknown command: {cmd}");
            usage();
        }
    }
    Ok(())
}
