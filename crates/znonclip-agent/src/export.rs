//! Token-budgeted context exporter for subagent prompts.
//!
//! Export context (files, clips) truncated to fit within a
//! token budget, so subagent prompts stay lean.

use anyhow::{Context, Result};

/// Rough token estimate: ~4 chars per token (standard heuristic).
fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

pub fn run(args: &[String]) -> Result<()> {
    let mut budget = 2000usize;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--budget" && i + 1 < args.len() {
            budget = args[i + 1].parse().context("Invalid --budget value")?;
            i += 2;
        } else {
            files.push(args[i].clone());
            i += 1;
        }
    }

    if files.is_empty() {
        eprintln!("Usage: znonclip-agent export --budget <tokens> <file>...");
        std::process::exit(1);
    }

    let mut used = 0;
    for file in &files {
        let content = std::fs::read_to_string(file)
            .with_context(|| format!("Failed to read {file}"))?;
        let tokens = estimate_tokens(&content);

        if used + tokens <= budget {
            println!("=== {file} ({tokens} tokens) ===");
            println!("{content}");
            used += tokens;
        } else {
            // Truncate to fit remaining budget
            let remaining = budget.saturating_sub(used);
            if remaining == 0 {
                eprintln!("Budget exhausted after {used} tokens; skipped {file}");
                break;
            }
            let max_chars = remaining * 4;
            let truncated: String = content.chars().take(max_chars).collect();
            println!("=== {file} (truncated to ~{remaining} tokens) ===");
            println!("{truncated}");
            println!("... [truncated]");
            used = budget;
            break;
        }
    }

    eprintln!("Used ~{used}/{budget} tokens across {} file(s).", files.len());
    Ok(())
}
