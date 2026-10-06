//! Cross-agent local IPC bus.
//!
//! Write a diff to a shared slot, another agent reads it in
//! microseconds instead of routing through git. Slots are files under the
//! agent dir, with atomic writes via rename.

use anyhow::{Context, Result};
use std::path::PathBuf;

fn slots_dir() -> Result<PathBuf> {
    let dir = crate::agent_dir()?.join("ipc-slots");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn slot_path(slot: &str) -> Result<PathBuf> {
    // Sanitize slot name to prevent path traversal
    let safe: String = slot
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        anyhow::bail!("Invalid slot name: {slot}");
    }
    Ok(slots_dir()?.join(format!("{safe}.slot")))
}

pub fn run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("Usage: znonclip-agent ipc <write|read|list> [...]");
        std::process::exit(1);
    }
    match args[0].as_str() {
        "write" => {
            if args.len() < 3 {
                eprintln!("Usage: znonclip-agent ipc write <slot> <content>");
                std::process::exit(1);
            }
            let path = slot_path(&args[1])?;
            let content = args[2..].join(" ");
            // Atomic write via temp file + rename
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &content)
                .with_context(|| format!("Failed to write slot {}", args[1]))?;
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
