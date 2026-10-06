//! Safe in-memory / slot diff viewer.
//!
//! Compare two IPC slots or a slot against a disk file
//! without touching git. Outputs unified diff syntax for auditor review.
//!
//! Usage:
//!   znonclip-agent diff slot:<name> <file>     - diff slot vs file
//!   znonclip-agent diff <file1> <file2>        - diff two files
//!   znonclip-agent diff slot:<a> slot:<b>     - diff two slots

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Read content from a slot: or file: specifier.
/// Bare paths are treated as files. Prefix with `slot:` for IPC slots.
fn read_spec(spec: &str) -> Result<(String, String)> {
    if let Some(slot) = spec.strip_prefix("slot:") {
        // Reuse the slot path logic from ipc module
        let safe: String = slot
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if safe.is_empty() {
            anyhow::bail!("Invalid slot name: {slot}");
        }
        let dir = crate::agent_dir()?.join("ipc-slots");
        let path = dir.join(format!("{safe}.slot"));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read slot {slot}"))?;
        Ok((format!("slot:{slot}"), content))
    } else {
        let content = std::fs::read_to_string(spec)
            .with_context(|| format!("Failed to read file {spec}"))?;
        Ok((spec.to_string(), content))
    }
}

/// A single diff operation.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    /// Line exists in both (context)
    Same(String),
    /// Line only in old (deletion)
    Del(String),
    /// Line only in new (addition)
    Add(String),
}

/// Compute a line-based diff using LCS (longest common subsequence).
/// Returns a vec of ops in order.
fn compute_diff(old_lines: &[&str], new_lines: &[&str]) -> Vec<Op> {
    let m = old_lines.len();
    let n = new_lines.len();

    // LCS DP table: lcs[i][j] = LCS length of old[i..] and new[j..]
    // Use a 2-row rolling array for memory efficiency
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];
    // Store the full table for backtracking (m*n, fine for reasonable files)
    let mut table = vec![vec![0usize; n + 1]; m + 1];

    for i in (0..m).rev() {
        for j in (0..n).rev() {
            if old_lines[i] == new_lines[j] {
                table[i][j] = table[i + 1][j + 1] + 1;
            } else {
                table[i][j] = table[i + 1][j].max(table[i][j + 1]);
            }
        }
        // Keep prev/curr updated (not strictly needed with full table, but harmless)
        std::mem::swap(&mut prev, &mut curr);
    }

    // Backtrack to produce ops
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < m && j < n {
        if old_lines[i] == new_lines[j] {
            ops.push(Op::Same(old_lines[i].to_string()));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push(Op::Del(old_lines[i].to_string()));
            i += 1;
        } else {
            ops.push(Op::Add(new_lines[j].to_string()));
            j += 1;
        }
    }
    while i < m {
        ops.push(Op::Del(old_lines[i].to_string()));
        i += 1;
    }
    while j < n {
        ops.push(Op::Add(new_lines[j].to_string()));
        j += 1;
    }

    ops
}

/// A hunk in unified diff format.
struct Hunk {
    old_start: usize, // 1-based
    old_len: usize,
    new_start: usize, // 1-based
    new_len: usize,
    lines: Vec<String>, // with ' ', '-', '+' prefixes
}

/// Group ops into hunks with context lines.
fn to_hunks(ops: &[Op], context: usize) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut i = 0;

    while i < ops.len() {
        // Skip over Same ops until we find a change
        if matches!(ops[i], Op::Same(_)) {
            i += 1;
            continue;
        }

        // Found a change at i. Expand backward for context.
        let hunk_start = i.saturating_sub(context);

        // Find the end: continue until we have `context` trailing Same ops
        // or we hit the end.
        let mut j = i;
        let mut trailing_same = 0;
        while j < ops.len() {
            match &ops[j] {
                Op::Same(_) => {
                    trailing_same += 1;
                    if trailing_same > context {
                        // We've collected enough trailing context; back up
                        j -= trailing_same - context;
                        break;
                    }
                }
                _ => {
                    trailing_same = 0;
                }
            }
            j += 1;
        }

        // Build the hunk
        let mut old_line = 1; // 1-based line numbers
        let mut new_line = 1;
        // Count lines before hunk_start to get starting numbers
        for k in 0..hunk_start {
            match &ops[k] {
                Op::Same(_) => {
                    old_line += 1;
                    new_line += 1;
                }
                Op::Del(_) => old_line += 1,
                Op::Add(_) => new_line += 1,
            }
        }

        let mut hunk_lines = Vec::new();
        let mut old_len = 0;
        let mut new_len = 0;
        for k in hunk_start..j.min(ops.len()) {
            match &ops[k] {
                Op::Same(s) => {
                    hunk_lines.push(format!(" {s}"));
                    old_len += 1;
                    new_len += 1;
                }
                Op::Del(s) => {
                    hunk_lines.push(format!("-{s}"));
                    old_len += 1;
                }
                Op::Add(s) => {
                    hunk_lines.push(format!("+{s}"));
                    new_len += 1;
                }
            }
        }

        hunks.push(Hunk {
            old_start: old_line,
            old_len,
            new_start: new_line,
            new_len,
            lines: hunk_lines,
        });

        i = j;
        // Skip the trailing context we already consumed as part of this hunk
        // (the next iteration will skip Same ops naturally)
    }

    hunks
}

pub fn run(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("Usage: znonclip-agent diff <old> <new>");
        eprintln!("  <old> and <new> can be file paths or slot:<name> for IPC slots.");
        eprintln!("  Example: znonclip-agent diff slot:proposed-patch src/main.rs");
        std::process::exit(1);
    }

    let (old_label, old_content) = read_spec(&args[0])?;
    let (new_label, new_content) = read_spec(&args[1])?;

    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    if old_lines == new_lines {
        println!("No differences between {old_label} and {new_label}.");
        return Ok(());
    }

    let ops = compute_diff(&old_lines, &new_lines);
    let hunks = to_hunks(&ops, 3);

    let old_count = old_lines.len();
    let new_count = new_lines.len();
    let added = ops.iter().filter(|o| matches!(o, Op::Add(_))).count();
    let removed = ops.iter().filter(|o| matches!(o, Op::Del(_))).count();

    println!("--- {old_label}");
    println!("+++ {new_label}");
    for hunk in hunks {
        println!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
        );
        for line in hunk.lines {
            println!("{line}");
        }
    }
    eprintln!(
        "{} line(s) in {}, {} line(s) in {} ({} addition(s), {} deletion(s))",
        old_count, old_label, new_count, new_label, added, removed
    );

    Ok(())
}

#[allow(dead_code)]
fn _unused_pathbuf() -> PathBuf {
    PathBuf::new()
}
