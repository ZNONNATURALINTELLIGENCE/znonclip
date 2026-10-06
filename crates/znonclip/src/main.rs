//! ZnonClip — macOS menu-bar clipboard history manager.
//!
//! Entry point: initializes logging and starts the AppKit application.
//!
//! ```text
//! znonclip              # run in the foreground (terminal stays attached)
//! znonclip --detach     # spawn in the background and return immediately
//! znonclip -d           # short form of --detach
//! ```

mod accessibility;
mod app;
mod autopaste;
mod clipboard;
mod hotkey;
mod instance;
mod launch;
mod predict;
mod privacy;
mod settings;
mod status_item;
mod storage;

use std::env;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use log::info;

fn main() {
    let raw: Vec<String> = env::args().collect();
    let args: Vec<String> = raw.iter().skip(1).cloned().collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    // Detach before logging/AppKit so the parent can exit cleanly.
    if let Some(pos) = args.iter().position(|a| a == "-d" || a == "--detach") {
        let mut child_args = args.clone();
        child_args.remove(pos);
        // Guard: never pass detach through (avoids accidental re-spawn loops).
        child_args.retain(|a| a != "-d" && a != "--detach");
        // Check (and release) the lock first so the user hears "already running"
        // instead of a silent second process that exits on its own.
        if let instance::Acquire::AlreadyRunning = instance::acquire() {
            eprintln!("znonclip: already running (one instance only)");
            std::process::exit(0);
        }
        detach_and_exit(&child_args);
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // One instance only: a second launch (terminal, --detach, login agent) exits here.
    let _instance_lock = match instance::acquire() {
        instance::Acquire::Acquired(f) => Some(f),
        instance::Acquire::AlreadyRunning => {
            info!("another ZnonClip instance is running — exiting");
            eprintln!("znonclip: already running (one instance only)");
            return;
        }
        instance::Acquire::Unavailable(e) => {
            log::warn!("single-instance lock unavailable ({e}); continuing");
            None
        }
    };

    if env::var_os(launch::AUTOSTART_ENV).is_some() {
        info!("ZnonClip starting (launchd autostart — no Terminal)");
    } else {
        info!("ZnonClip starting");
    }
    app::run();
}

fn print_help() {
    let name = env::args()
        .next()
        .and_then(|p| {
            PathBuf::from(p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "znonclip".into());

    println!(
        "\
ZnonClip — macOS menu-bar clipboard history

USAGE:
    {name} [OPTIONS]

OPTIONS:
    -d, --detach     Run in the background (terminal can close)
    -h, --help       Show this help

EXAMPLES:
    {name}           Foreground (logs in this terminal)
    {name} -d        Detached / independent process
    {name} --detach

Detached logs: ~/Library/Logs/ZnonClip/znonclip.log
Stop:          pkill -x znonclip
"
    );
}

/// Re-spawn this binary without `--detach`, fully independent of the terminal.
fn detach_and_exit(child_args: &[String]) -> ! {
    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("znonclip: cannot resolve executable path: {e}");
            std::process::exit(1);
        }
    };

    let mut cmd = Command::new(&exe);
    cmd.args(child_args);
    cmd.stdin(Stdio::null());
    cmd.env_remove("ZNONCLIP_DETACH_CHILD"); // clean slate

    // Prefer a log file so detached runs are still diagnosable.
    match open_detach_log() {
        Ok(file) => match file.try_clone() {
            Ok(err_file) => {
                cmd.stdout(Stdio::from(file));
                cmd.stderr(Stdio::from(err_file));
            }
            Err(_) => {
                cmd.stdout(Stdio::from(file));
                cmd.stderr(Stdio::null());
            }
        },
        Err(e) => {
            eprintln!("znonclip: warning: could not open log file ({e}); discarding output");
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
    }

    // Detach from the controlling terminal (new session).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // SAFETY: setsid has no preconditions beyond process context.
                if setsid() == -1 {
                    // Non-fatal: process still runs, may receive SIGHUP if
                    // the parent shell exits without disown — rare with null stdio.
                }
                Ok(())
            });
        }
    }

    match cmd.spawn() {
        Ok(child) => {
            let log_hint = detach_log_path()
                .map(|p| format!("\n  logs: {}", p.display()))
                .unwrap_or_default();
            eprintln!(
                "ZnonClip started in background (pid {}){}",
                child.id(),
                log_hint
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("znonclip: failed to start detached process: {e}");
            std::process::exit(1);
        }
    }
}

fn detach_log_path() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("ZnonClip")
            .join("znonclip.log"),
    )
}

fn open_detach_log() -> std::io::Result<std::fs::File> {
    let path = detach_log_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(unix)]
extern "C" {
    fn setsid() -> i32;
}
