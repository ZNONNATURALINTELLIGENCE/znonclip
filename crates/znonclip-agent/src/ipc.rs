//! Cross-agent local IPC bus.
//!
//! Write text to a named slot; another agent on the same machine reads it,
//! with no git round-trip. Slots are owner-only files (`0600`) under the agent
//! dir, written atomically (temp file + rename). Content is passed through the
//! secret scrubber unless `--raw` is given.

use anyhow::{Context, Result};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

fn slots_dir() -> Result<PathBuf> {
    let dir = crate::agent_dir()?.join("ipc-slots");
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Ok(dir)
}

/// Slot names are `[A-Za-z0-9_-]+`. Anything else is rejected outright (not
/// stripped), so `../x` and `x` can never silently become the same slot.
pub fn slot_path(slot: &str) -> Result<PathBuf> {
    let valid = !slot.is_empty()
        && slot.len() <= 128
        && slot.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        anyhow::bail!("Invalid slot name {slot:?}: use letters, digits, '-' and '_'");
    }
    Ok(slots_dir()?.join(format!("{slot}.slot")))
}

pub fn run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("Usage: znonclip-agent ipc <write|read|list> [...]");
        std::process::exit(1);
    }
    match args[0].as_str() {
        "write" => {
            if args.len() < 2 {
                eprintln!("Usage: znonclip-agent ipc write <slot> [--raw] [content]  (content from stdin if omitted)");
                std::process::exit(1);
            }
            let path = slot_path(&args[1])?;
            let raw = args[2..].iter().any(|a| a == "--raw");
            let words: Vec<&String> = args[2..].iter().filter(|a| *a != "--raw").collect();
            let mut content = if words.is_empty() {
                let mut stdin = std::io::stdin();
                if stdin.is_terminal() {
                    anyhow::bail!("no content: pass it as arguments or pipe it on stdin");
                }
                let mut s = String::new();
                stdin.read_to_string(&mut s)?;
                s
            } else {
                words.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
            };
            if !raw {
                content = znonclip_core::scrub_secrets(&content);
            }
            // Atomic, owner-only write: a per-process temp name, then rename.
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("Failed to write slot {}", args[1]))?;
            f.write_all(content.as_bytes())?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            println!("Wrote {} bytes to slot '{}'", content.len(), args[1]);
        }
        "read" => {
            if args.len() < 2 {
                eprintln!("Usage: znonclip-agent ipc read <slot>");
                std::process::exit(1);
            }
            let path = slot_path(&args[1])?;
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read slot {}", args[1]))?;
            print!("{content}");
        }
        "list" => {
            let dir = slots_dir()?;
            let mut slots: Vec<String> = std::fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
                .collect();
            slots.sort();
            for s in slots {
                println!("{s}");
            }
        }
        _ => {
            eprintln!("Unknown ipc subcommand: {}", args[0]);
            std::process::exit(1);
        }
    }
    Ok(())
}
