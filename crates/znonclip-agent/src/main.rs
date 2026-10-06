//! znonclip-agent: agent tools for ZnonClip.
//!
//! - `ipc`: Cross-agent local IPC bus (write/read shared slots)
//! - `stage`: Patch-staging gate (syntax-check code before it touches the filesystem)
//! - `export`: Token-budgeted context exporter for subagent prompts
//! - `diff`: Safe slot/file diff viewer (unified diff, no git)
//! - `snap`: Ephemeral snapshot & rollback for risky refactors

use anyhow::Result;
use std::path::PathBuf;

mod diff;
mod ipc;
mod snap;
mod stage;
mod export;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: znonclip-agent <ipc|stage|export|diff|snap> [...]");
        eprintln!("  ipc write <slot> <content>  - Write to a shared IPC slot");
        eprintln!("  ipc read <slot>              - Read from a shared IPC slot");
        eprintln!("  ipc list                    - List all slots");
        eprintln!("  stage <file>                - Syntax-check a file before applying");
        eprintln!("  stage --as <path> < stdin   - Syntax-check stdin as <path>");
        eprintln!("  export --budget <tokens> <file>... - Export context within token budget");
        eprintln!("  diff <old> <new>            - Unified diff (slot:<name> or file path)");
        eprintln!("  snap create <name> <file>... - Snapshot files");
        eprintln!("  snap rollback <name>         - Restore files from snapshot");
        eprintln!("  snap list                    - List snapshots");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "ipc" => ipc::run(&args[2..]),
        "stage" => stage::run(&args[2..]),
        "export" => export::run(&args[2..]),
        "diff" => diff::run(&args[2..]),
        "snap" => snap::run(&args[2..]),
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
}

/// Base directory for agent-local state. Owner-only (0700): it holds
/// snapshot copies of files and slot payloads.
fn agent_dir() -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = directories::ProjectDirs::from("com", "znonclip", "ZnonClip")
        .map(|d| d.data_dir().join("agent"))
        .unwrap_or_else(|| PathBuf::from(".znonclip-agent"));
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Ok(dir)
}
