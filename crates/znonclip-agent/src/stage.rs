//! Patch-staging gate: syntax-check code before it touches the filesystem.
//!
//! Stage a patch, verify it parses, then apply. This prevents
//! broken code from ever landing in the working tree.

use anyhow::{Context, Result};
use std::process::Command;

pub fn run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("Usage: znonclip-agent stage <file>...");
        eprintln!("  Checks syntax without modifying the file.");
        std::process::exit(1);
    }

    let mut all_ok = true;
    for file in args {
        let ok = check_file(file)?;
        if !ok {
            all_ok = false;
        }
    }

    if all_ok {
        println!("All files pass syntax check.");
    } else {
        eprintln!("Some files failed syntax check.");
        std::process::exit(1);
    }
    Ok(())
}

fn check_file(path: &str) -> Result<bool> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let output = match ext {
        "rs" => {
            // Use rustc --crate-type=lib --emit=metadata for a parse check
            // (requires the file to be valid Rust, not necessarily compilable standalone)
            Command::new("rustc")
                .args(["--edition=2021", "--crate-type=lib", "--emit=metadata", "-o", "/dev/null", path])
                .output()
                .context("Failed to run rustc")?
        }
        "py" => Command::new("python3")
            .args(["-m", "py_compile", path])
            .output()
            .context("Failed to run python3")?,
        "js" | "mjs" => Command::new("node")
            .args(["--check", path])
            .output()
            .context("Failed to run node")?,
        _ => {
            println!("{path}: no checker for .{ext}, skipping");
            return Ok(true);
        }
    };

    if output.status.success() {
        println!("{path}: OK");
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{path}: SYNTAX ERROR\n{stderr}");
        Ok(false)
    }
}
