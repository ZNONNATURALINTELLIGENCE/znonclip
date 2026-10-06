//! znonclip-agent: agent tools for ZnonClip.
//!
//! - `ipc`: Cross-agent local IPC bus (write/read shared slots)
//! - `stage`: Patch-staging gate (syntax-check code before it touches the filesystem)
//! - `export`: Token-budgeted context exporter for subagent prompts

use anyhow::Result;
use std::path::PathBuf;

mod ipc;
mod stage;
mod export;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: znonclip-agent <ipc|stage|export> [...]");
        eprintln!("  ipc write <slot> <content>  - Write to a shared IPC slot");
        eprintln!("  ipc read <slot>              - Read from a shared IPC slot");
        eprintln!("  ipc list                    - List all slots");
        eprintln!("  stage <file>                - Syntax-check a file before applying");
        eprintln!("  export --budget <tokens> <file>... - Export context within token budget");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "ipc" => ipc::run(&args[2..]),
        "stage" => stage::run(&args[2..]),
        "export" => export::run(&args[2..]),
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
}

/// Base directory for agent-local state.
fn agent_dir() -> Result<PathBuf> {
    let dir = directories::ProjectDirs::from("com", "znonclip", "ZnonClip")
        .map(|d| d.data_dir().join("agent"))
        .unwrap_or_else(|| PathBuf::from(".znonclip-agent"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
