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

/// Read content from a `slot:` or file specifier, with known secrets
/// redacted: the output goes into agent transcripts.
fn read_spec(spec: &str) -> Result<(String, String)> {
    let (label, content) = if let Some(slot) = spec.strip_prefix("slot:") {
        let path = crate::ipc::slot_path(slot)?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read slot {slot}"))?;
        (format!("slot:{slot}"), content)
    } else {
        let content = std::fs::read_to_string(spec)
            .with_context(|| format!("Failed to read file {spec}"))?;
        (spec.to_string(), content)
    };
    Ok((label, znonclip_core::scrub_secrets(&content)))
}

/// The LCS table is (m+1) x (n+1) cells; refuse inputs that would need more.
const MAX_TABLE_CELLS: usize = 16_000_000;

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

    // LCS DP table: table[i][j] = LCS length of old[i..] and new[j..]. The
    // full table is kept for backtracking; `run` caps its size.
    let mut table = vec![vec![0usize; n + 1]; m + 1];

    for i in (0..m).rev() {
        for j in (0..n).rev() {
            if old_lines[i] == new_lines[j] {
                table[i][j] = table[i + 1][j + 1] + 1;
            } else {
                table[i][j] = table[i + 1][j].max(table[i][j + 1]);
            }
        }
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
    // End of the previous hunk: leading context never repeats its lines.
    let mut last_end = 0;

    while i < ops.len() {
        // Skip over Same ops until we find a change
        if matches!(ops[i], Op::Same(_)) {
            i += 1;
            continue;
        }

        // Found a change at i. Expand backward for context.
        let hunk_start = i.saturating_sub(context).max(last_end);

        // Find the end: continue until we have `context` trailing Same ops
        // or we hit the end.
        let mut j = i;
        let mut trailing_same = 0;
        while j < ops.len() {
            match &ops[j] {
                Op::Same(_) => {
                    trailing_same += 1;
                    if trailing_same > context {
                        // ops[j] is one Same past the context: end the hunk
                        // before it, keeping exactly `context` trailing lines.
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
        last_end = j;
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
    let cells = (old_lines.len() + 1).saturating_mul(new_lines.len() + 1);
    if cells > MAX_TABLE_CELLS {
        anyhow::bail!(
            "inputs too large to diff in memory ({} x {} lines); use git diff",
            old_lines.len(),
            new_lines.len()
        );
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn hunks_keep_exactly_three_context_lines() {
        let old = lines(20);
        let mut new = old.clone();
        new[10] = "changed".into();
        let o: Vec<&str> = old.iter().map(|s| s.as_str()).collect();
        let n: Vec<&str> = new.iter().map(|s| s.as_str()).collect();
        let hunks = to_hunks(&compute_diff(&o, &n), 3);
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        // 3 before + 1 deletion + 1 addition + 3 after.
        assert_eq!(h.lines.len(), 8, "{:?}", h.lines);
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (8, 7, 8, 7));
    }

    #[test]
    fn close_hunks_do_not_repeat_lines() {
        let old = lines(20);
        let mut new = old.clone();
        new[5] = "a".into();
        new[9] = "b".into();
        let o: Vec<&str> = old.iter().map(|s| s.as_str()).collect();
        let n: Vec<&str> = new.iter().map(|s| s.as_str()).collect();
        let hunks = to_hunks(&compute_diff(&o, &n), 3);
        let shown: Vec<&String> = hunks.iter().flat_map(|h| h.lines.iter()).collect();
        let context: Vec<&&String> = shown.iter().filter(|l| l.starts_with(' ')).collect();
        let mut dedup = context.clone();
        dedup.dedup();
        assert_eq!(context.len(), dedup.len(), "context repeated: {shown:?}");
    }
}
