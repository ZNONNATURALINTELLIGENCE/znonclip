//! znonclip-cli: terminal interface for ZnonClip.
//!
//! Pin sprint rules, stash research JSON, and query clipboard history
//! without leaving the terminal.

use anyhow::Result;
use znonclip_core::{scrub_secrets, ClipStore};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: znonclip-cli <pin|stash|recent|scrub> [...]");
        eprintln!("  pin <text>     - Pin text as an active rule");
        eprintln!("  stash <text>   - Stash text (e.g., research JSON)");
        eprintln!("  recent [n]     - Show recent clips (default 10)");
        eprintln!("  scrub <text>   - Scrub secrets from text");
        std::process::exit(1);
    }

    let store = ClipStore::open_default()?;

    match args[1].as_str() {
        "pin" => {
            let text = args[2..].join(" ");
            let id = store.insert(&text, "text")?;
            store.set_pinned(id, true)?;
            println!("Pinned clip #{id}");
        }
        "stash" => {
            let text = args[2..].join(" ");
            // Scrub secrets before stashing
            let clean = scrub_secrets(&text);
            let id = store.insert(&clean, "stash")?;
            println!("Stashed clip #{id}");
        }
        "recent" => {
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
            for item in store.recent(n)? {
                let pin = if item.pinned { "[PIN] " } else { "" };
                let preview: String = item.content.chars().take(80).collect();
                println!("#{id} {pin}{preview}", id = item.id);
            }
        }
        "scrub" => {
            let text = args[2..].join(" ");
            println!("{}", scrub_secrets(&text));
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }

    Ok(())
}
