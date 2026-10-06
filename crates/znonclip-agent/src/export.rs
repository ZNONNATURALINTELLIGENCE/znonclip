//! Token-budgeted context exporter for subagent prompts.
//!
//! Export context (files, clips) truncated to fit within a
//! token budget, so subagent prompts stay lean.
//!
//! Every file is passed through `scrub_secrets` before it is budgeted or
//! truncated: the output is meant for a model's context window, and
//! truncating first could cut a key in half and leak the fragment.

use std::io::Write;

use anyhow::{Context, Result};
use znonclip_core::scrub_secrets;

/// Rough token estimate: ~4 characters per token. Counted in characters,
/// the same unit truncation uses, so non-ASCII text cannot overrun the budget.
fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Extra characters read past the budget so a secret straddling the cut is
/// still whole when the scrubber sees it.
const SCRUB_MARGIN_CHARS: usize = 4096;

/// Read at most enough of `path` for `max_chars` characters (UTF-8 is at most
/// 4 bytes a character), never the whole of a huge file. Returns the text and
/// whether the file was longer.
fn read_prefix(path: &str, max_chars: usize) -> Result<(String, bool)> {
    use std::io::Read;
    let max_bytes = max_chars.saturating_mul(4) as u64;
    let file = std::fs::File::open(path).with_context(|| format!("Failed to read {path}"))?;
    let mut buf = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut buf)?;
    let longer = buf.len() as u64 > max_bytes;
    buf.truncate(max_bytes as usize);
    // Drop a partial multi-byte character at the cut, then decode.
    let text = match String::from_utf8(buf) {
        Ok(t) => t,
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
    };
    Ok((text, longer))
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

    export(&files, budget, &mut std::io::stdout().lock())
}

/// Write the scrubbed, budgeted export of `files` to `out`.
fn export(files: &[String], budget: usize, out: &mut impl Write) -> Result<()> {
    let mut used = 0;
    for file in files {
        let remaining_chars = budget.saturating_sub(used).saturating_mul(4);
        let (raw, longer) = read_prefix(file, remaining_chars + SCRUB_MARGIN_CHARS)?;
        let content = scrub_secrets(&raw);
        if content != raw {
            eprintln!("Redacted secrets in {file}");
        }
        let tokens = estimate_tokens(&content);

        if !longer && used + tokens <= budget {
            writeln!(out, "=== {file} ({tokens} tokens) ===")?;
            writeln!(out, "{content}")?;
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
            writeln!(out, "=== {file} (truncated to ~{remaining} tokens) ===")?;
            writeln!(out, "{truncated}")?;
            writeln!(out, "... [truncated]")?;
            used = budget;
            break;
        }
    }

    eprintln!("Used ~{used}/{budget} tokens across {} file(s).", files.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export_string(content: &str, budget: usize) -> String {
        let dir = std::env::temp_dir().join(format!(
            "znonclip-export-test-{}-{budget}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("leaky.txt");
        std::fs::write(&path, content).unwrap();
        let mut out = Vec::new();
        export(&[path.to_string_lossy().into_owned()], budget, &mut out).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn export_redacts_secrets() {
        let key = format!("AKIA{}", "ABCDEFGHIJKLMNOP");
        let out = export_string(&format!("aws = {key}\n"), 2000);
        assert!(!out.contains(&key));
        assert!(out.contains("[REDACTED:aws-key]"));
    }

    #[test]
    fn hyphenated_key_cut_by_the_budget_is_still_scrubbed() {
        let key = format!("{}{}", ["sk", "proj-"].join("-"), "Zx9_".repeat(12));
        let out = export_string(&format!("k={key} and more text after it"), 3);
        assert!(!out.contains("sk-proj-Zx9"), "{out}");
    }

    #[test]
    fn budget_counts_characters_not_bytes() {
        // 40 four-byte characters = 10 tokens by characters (40 by bytes).
        let out = export_string(&"🦀".repeat(40), 10);
        assert!(!out.contains("truncated"), "{out}");
    }

    #[test]
    fn truncation_cannot_leak_a_key_fragment() {
        // Budget lands mid-key: truncating before scrubbing would emit a prefix
        // of the key that no longer matches the AWS pattern.
        let key = format!("AKIA{}", "ABCDEFGHIJKLMNOP");
        let out = export_string(&format!("aws={key} tail text after the key"), 4);
        assert!(!out.contains("AKIAABCD"));
    }
}
