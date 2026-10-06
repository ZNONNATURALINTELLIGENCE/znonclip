//! Patch-staging check: confirm a buffer or file parses before you write it.
//!
//! Supported: Rust (`rustc --emit=metadata`, so it is type-checked as a
//! standalone crate and files that rely on sibling modules fail), Python
//! (`ast.parse`, nothing written), JavaScript (`node --check`). Any other
//! file type is an error, never a silent pass. This command only checks; it
//! does not write or apply anything.

use anyhow::{Context, Result};
use std::process::Command;

pub fn run(args: &[String]) -> Result<()> {
    // Check for --as <path> flag (stdin mode)
    let mut as_path: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--as" && i + 1 < args.len() {
            as_path = Some(args[i + 1].clone());
            i += 2;
        } else {
            files.push(args[i].clone());
            i += 1;
        }
    }

    // Stdin mode: read content from stdin, check syntax as if it were the --as path
    if let Some(target) = as_path {
        use std::io::Read;
        let mut content = String::new();
        std::io::stdin()
            .read_to_string(&mut content)
            .context("Failed to read from stdin")?;
        let ok = check_content(&target, &content)?;
        if ok {
            println!("{target}: OK (from stdin)");
        } else {
            eprintln!("{target}: SYNTAX ERROR (from stdin)");
            std::process::exit(1);
        }
        return Ok(());
    }

    if files.is_empty() {
        eprintln!("Usage: znonclip-agent stage <file>...");
        eprintln!("       cat new_code.rs | znonclip-agent stage --as <path>");
        eprintln!("  Checks syntax without modifying the file.");
        std::process::exit(1);
    }

    let mut all_ok = true;
    for file in &files {
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

/// Check syntax of in-memory content as if it were a file at `path`.
/// Uses the file extension to pick the checker. Writes to a temp file
/// for checkers that require a file path.
fn check_content(path: &str, content: &str) -> Result<bool> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // For checkers that need a file, write to a temp file with the right extension
    let tmp_path = match ext {
        "rs" | "py" | "js" | "mjs" => {
            let tmp = std::env::temp_dir().join(format!(
                "znonclip-stage-{}.{}",
                std::process::id(),
                ext
            ));
            std::fs::write(&tmp, content)?;
            Some(tmp)
        }
        _ => None,
    };

    let check_path = tmp_path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());

    let result = check_file_inner(&check_path, ext);

    // Clean up temp file
    if let Some(tmp) = tmp_path {
        let _ = std::fs::remove_file(tmp);
    }

    result
}

fn check_file(path: &str) -> Result<bool> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    check_file_inner(path, ext)
}

fn check_file_inner(path: &str, ext: &str) -> Result<bool> {

    let output = match ext {
        "rs" => {
            // Use rustc --crate-type=lib --emit=metadata for a parse check
            // (requires the file to be valid Rust, not necessarily compilable standalone)
            Command::new("rustc")
                .args(["--edition=2021", "--crate-type=lib", "--emit=metadata", "-o", "/dev/null", path])
                .output()
                .context("Failed to run rustc")?
        }
        // ast.parse writes no __pycache__ next to the file, unlike py_compile.
        "py" => Command::new("python3")
            .args([
                "-c",
                "import ast,sys; ast.parse(open(sys.argv[1],encoding='utf-8').read(), sys.argv[1])",
                path,
            ])
            .output()
            .context("Failed to run python3")?,
        "js" | "mjs" => Command::new("node")
            .args(["--check", path])
            .output()
            .context("Failed to run node")?,
        _ => {
            eprintln!("{path}: no checker for .{ext} (supported: rs, py, js, mjs)");
            return Ok(false);
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
