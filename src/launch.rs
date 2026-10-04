//! Auto-launch at login.
//!
//! Strategy (critical for production CLI installs):
//!
//! * **Proper `.app` bundle** → `SMAppService` (Login Items, no Terminal).
//! * **Bare binary** (`cargo install`, `~/.cargo/bin/znonclip`) → **only** a
//!   user LaunchAgent under `~/Library/LaunchAgents/`.
//!
//! Never register a bare Unix executable with `SMAppService` / Login Items:
//! macOS often re-opens those via **Terminal.app** at login.
//!
//! LaunchAgents are started by `launchd` in the Aqua session — already
//! detached from any terminal. Do **not** pass `--detach` there (nested
//! spawn is unnecessary and can leave a short-lived console-looking job).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use log::{info, warn};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSBundle, NSString};

#[link(name = "ServiceManagement", kind = "framework")]
extern "C" {}

/// LaunchAgent label / plist basename (must match reverse-DNS of the app).
const AGENT_LABEL: &str = "com.znonclip.app";

/// Env flag set by the LaunchAgent so the process knows it was auto-started.
pub const AUTOSTART_ENV: &str = "ZNONCLIP_AUTOSTART";

/// Status from `SMAppService.status` (ServiceManagement), plus LaunchAgent mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(isize)]
pub enum LoginItemStatus {
    NotRegistered = 0,
    Enabled = 1,
    RequiresApproval = 2,
    NotFound = 3,
    Unknown = -1,
}

impl LoginItemStatus {
    pub fn from_raw(v: isize) -> Self {
        match v {
            0 => Self::NotRegistered,
            1 => Self::Enabled,
            2 => Self::RequiresApproval,
            3 => Self::NotFound,
            _ => Self::Unknown,
        }
    }

    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::NotRegistered => "not registered",
            Self::Enabled => "enabled",
            Self::RequiresApproval => "requires approval in System Settings",
            Self::NotFound => "app not found (install .app to Applications)",
            Self::Unknown => "unknown",
        }
    }
}

/// True when this process lives inside `Something.app/Contents/MacOS/…`.
///
/// Only then is `SMAppService.mainApp` / Login Items the right API. Bare
/// binaries must use a LaunchAgent or macOS may open Terminal at login.
pub fn running_inside_app_bundle() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    path_is_inside_app_bundle(&exe)
}

fn path_is_inside_app_bundle(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.ends_with(".app"))
    })
}

/// Query whether launch-at-login is active.
pub fn status() -> LoginItemStatus {
    if running_inside_app_bundle() {
        if let Some(s) = sm_status() {
            if s.is_enabled() || matches!(s, LoginItemStatus::RequiresApproval) {
                return s;
            }
        }
    }
    if launch_agent_is_enabled() {
        LoginItemStatus::Enabled
    } else if running_inside_app_bundle() {
        sm_status().unwrap_or(LoginItemStatus::NotRegistered)
    } else {
        LoginItemStatus::NotRegistered
    }
}

/// Enable launch at login.
///
/// * `.app` → SMAppService (and clear any leftover LaunchAgent).
/// * bare binary → LaunchAgent only (never SMAppService — avoids Terminal).
pub fn enable() -> Result<LoginItemStatus, String> {
    if running_inside_app_bundle() {
        match sm_enable() {
            Ok(st) if st.is_enabled() || matches!(st, LoginItemStatus::RequiresApproval) => {
                let _ = disable_launch_agent();
                return Ok(st);
            }
            Ok(st) => {
                info!(
                    "SMAppService enable returned {} — LaunchAgent fallback",
                    st.describe()
                );
            }
            Err(e) => {
                info!("SMAppService enable failed ({e}) — LaunchAgent fallback");
            }
        }
    } else {
        info!("bare binary — using LaunchAgent for login (no SMAppService / no Terminal)");
        // Best-effort: clear any accidental prior SM registration for this binary.
        let _ = sm_disable();
    }

    enable_launch_agent()?;
    Ok(LoginItemStatus::Enabled)
}

/// Disable launch at login (both mechanisms).
pub fn disable() -> Result<LoginItemStatus, String> {
    let mut errs: Vec<String> = Vec::new();

    if running_inside_app_bundle() {
        if let Err(e) = sm_disable() {
            warn!("SMAppService disable: {e}");
            errs.push(e);
        }
    } else {
        // Still try unregister in case an older build registered SM for a bare binary.
        let _ = sm_disable();
    }

    if let Err(e) = disable_launch_agent() {
        warn!("LaunchAgent disable: {e}");
        errs.push(e);
    }

    let st = status();
    if st.is_enabled() {
        Err(if errs.is_empty() {
            "failed to disable launch at login".into()
        } else {
            errs.join("; ")
        })
    } else {
        Ok(st)
    }
}

/// Enable or disable based on the desired flag.
pub fn set_enabled(want: bool) -> Result<LoginItemStatus, String> {
    if want {
        enable()
    } else {
        disable()
    }
}

// ── SMAppService (bundled .app only) ────────────────────────────────────────

fn sm_status() -> Option<LoginItemStatus> {
    with_main_app(|service| {
        if !responds_to(service, sel!(status)) {
            return LoginItemStatus::Unknown;
        }
        // SAFETY: -status returns SMAppServiceStatus (NSInteger).
        let raw: isize = unsafe { msg_send![service, status] };
        LoginItemStatus::from_raw(raw)
    })
}

fn sm_enable() -> Result<LoginItemStatus, String> {
    with_main_app(|service| {
        if !responds_to(service, sel!(registerAndReturnError:)) {
            return Err("SMAppService register API unavailable".into());
        }
        let mut err: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: registerAndReturnError: takes NSError * __autoreleasing *.
        let ok: Bool = unsafe { msg_send![service, registerAndReturnError: &mut err] };
        let st = read_status(service);
        if ok.as_bool() {
            info!("SMAppService register ok — status={}", st.describe());
            Ok(st)
        } else {
            let msg = nserror_description(err)
                .unwrap_or_else(|| format!("register failed (status={})", st.describe()));
            warn!("SMAppService register failed: {msg}");
            Err(msg)
        }
    })
    .unwrap_or_else(|| Err("SMAppService unavailable".into()))
}

fn sm_disable() -> Result<LoginItemStatus, String> {
    with_main_app(|service| {
        if !responds_to(service, sel!(unregisterAndReturnError:)) {
            return Err("SMAppService unregister API unavailable".into());
        }
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let ok: Bool = unsafe { msg_send![service, unregisterAndReturnError: &mut err] };
        let st = read_status(service);
        if ok.as_bool() {
            info!("SMAppService unregister ok — status={}", st.describe());
            Ok(st)
        } else {
            let msg = nserror_description(err)
                .unwrap_or_else(|| format!("unregister failed (status={})", st.describe()));
            // Not registered is fine when disabling.
            if st == LoginItemStatus::NotRegistered || st == LoginItemStatus::NotFound {
                return Ok(st);
            }
            warn!("SMAppService unregister failed: {msg}");
            Err(msg)
        }
    })
    .unwrap_or_else(|| Err("SMAppService unavailable".into()))
}

fn read_status(service: &AnyObject) -> LoginItemStatus {
    if !responds_to(service, sel!(status)) {
        return LoginItemStatus::Unknown;
    }
    let raw: isize = unsafe { msg_send![service, status] };
    LoginItemStatus::from_raw(raw)
}

fn responds_to(obj: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: is on NSObject.
    let ok: Bool = unsafe { msg_send![obj, respondsToSelector: selector] };
    ok.as_bool()
}

fn ensure_framework_loaded() {
    if AnyClass::get(c"SMAppService").is_some() {
        return;
    }
    let path = NSString::from_str("/System/Library/Frameworks/ServiceManagement.framework");
    if let Some(bundle) = NSBundle::bundleWithPath(&path) {
        // SAFETY: loading a system framework by path is well-defined.
        let loaded = unsafe { bundle.load() };
        if loaded {
            info!("loaded ServiceManagement.framework");
        }
    }
}

/// Resolve `SMAppService` for the main app.
///
/// Recent macOS: `+[SMAppService mainAppService]`
/// Older docs / SDKs: `+[SMAppService mainApp]`
fn with_main_app<T>(f: impl FnOnce(&AnyObject) -> T) -> Option<T> {
    ensure_framework_loaded();
    let cls = AnyClass::get(c"SMAppService")?;

    let service: Option<Retained<AnyObject>> = {
        // SAFETY: class objects respond to NSObject protocol methods.
        let has_new: Bool =
            unsafe { msg_send![cls, respondsToSelector: sel!(mainAppService)] };
        if has_new.as_bool() {
            // SAFETY: +mainAppService returns SMAppService* (may be nil).
            let svc: Option<Retained<AnyObject>> = unsafe { msg_send![cls, mainAppService] };
            if svc.is_some() {
                info!("using +[SMAppService mainAppService]");
            }
            svc
        } else {
            let has_old: Bool =
                unsafe { msg_send![cls, respondsToSelector: sel!(mainApp)] };
            if has_old.as_bool() {
                // SAFETY: +mainApp returns SMAppService* (may be nil).
                let svc: Option<Retained<AnyObject>> = unsafe { msg_send![cls, mainApp] };
                if svc.is_some() {
                    info!("using +[SMAppService mainApp]");
                }
                svc
            } else {
                None
            }
        }
    };

    let service = match service {
        Some(s) => s,
        None => {
            warn!("SMAppService main-app accessor not available (tried mainAppService, mainApp)");
            return None;
        }
    };
    Some(f(&service))
}

fn nserror_description(err: *mut AnyObject) -> Option<String> {
    if err.is_null() {
        return None;
    }
    // SAFETY: err is NSError* from the out-parameter.
    let desc: Option<Retained<NSString>> = unsafe { msg_send![&*err, localizedDescription] };
    desc.map(|s| s.to_string())
}

// ── LaunchAgent (bare binary / CLI production install) ──────────────────────

fn agent_plist_path() -> Result<PathBuf, String> {
    let home = dirs_home().ok_or_else(|| "could not resolve home directory".to_string())?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{AGENT_LABEL}.plist")))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn current_executable() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    fs::canonicalize(&exe).or_else(|_| Ok::<_, String>(exe))
}

fn launch_agent_log_path() -> Option<PathBuf> {
    let home = dirs_home()?;
    Some(
        home.join("Library")
            .join("Logs")
            .join("ZnonClip")
            .join("znonclip.log"),
    )
}

/// Build LaunchAgent plist.
///
/// Important: run the binary **directly** (no shell, no `--detach`).
/// `launchd` already starts jobs outside Terminal; nesting `--detach` is
/// what previously left confusing terminal-looking sessions.
fn launch_agent_plist_xml(program: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let program_xml = esc(program);
    let log_xml = launch_agent_log_path()
        .and_then(|p| p.to_str().map(esc))
        .unwrap_or_else(|| "/dev/null".into());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program_xml}</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>{AUTOSTART_ENV}</key>
		<string>1</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>StandardOutPath</key>
	<string>{log_xml}</string>
	<key>StandardErrorPath</key>
	<string>{log_xml}</string>
	<key>WorkingDirectory</key>
	<string>/tmp</string>
</dict>
</plist>
"#
    )
}

fn uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions.
    unsafe { getuid() }
}

fn launch_agent_is_enabled() -> bool {
    let Ok(path) = agent_plist_path() else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    let domain = format!("gui/{}", uid());
    let service = format!("{domain}/{AGENT_LABEL}");
    let out = Command::new("launchctl")
        .args(["print", &service])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            return true;
        }
    }
    // Plist on disk = user turned it on (may apply next login if not loaded).
    true
}

fn enable_launch_agent() -> Result<(), String> {
    let exe = current_executable()?;
    if path_is_inside_app_bundle(&exe) {
        // Bundled apps should use SMAppService; still allow agent as last resort.
        info!("writing LaunchAgent for bundled binary at {}", exe.display());
    }
    let exe_str = exe
        .to_str()
        .ok_or_else(|| "executable path is not UTF-8".to_string())?;
    let plist_path = agent_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create LaunchAgents dir: {e}"))?;
    }
    if let Some(log) = launch_agent_log_path() {
        if let Some(dir) = log.parent() {
            let _ = fs::create_dir_all(dir);
        }
    }

    let xml = launch_agent_plist_xml(exe_str);
    {
        let mut f = fs::File::create(&plist_path)
            .map_err(|e| format!("write LaunchAgent plist: {e}"))?;
        f.write_all(xml.as_bytes())
            .map_err(|e| format!("write LaunchAgent plist: {e}"))?;
    }
    info!(
        "wrote LaunchAgent {} → {} (direct launchd start, no Terminal)",
        plist_path.display(),
        exe_str
    );

    let domain = format!("gui/{}", uid());
    let service = format!("{domain}/{AGENT_LABEL}");
    let plist_str = plist_path
        .to_str()
        .ok_or_else(|| "plist path is not UTF-8".to_string())?;

    // Unload old definition so ProgramArguments / paths refresh.
    let _ = Command::new("launchctl")
        .args(["bootout", &service])
        .output();
    // Also try path-based bootout (older style).
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, plist_str])
        .output();

    let bootstrap = Command::new("launchctl")
        .args(["bootstrap", &domain, plist_str])
        .output()
        .map_err(|e| format!("launchctl bootstrap: {e}"))?;

    if !bootstrap.status.success() {
        let stderr = String::from_utf8_lossy(&bootstrap.stderr);
        if stderr.contains("already bootstrapped") || stderr.contains("already loaded") {
            let _ = Command::new("launchctl")
                .args(["enable", &service])
                .output();
            // Kickload replacement: bootout + bootstrap already attempted.
            info!("LaunchAgent already present; enabled {service}");
            return Ok(());
        }
        return Err(format!(
            "launchctl bootstrap failed: {}",
            stderr.trim().if_empty("no stderr")
        ));
    }

    let _ = Command::new("launchctl")
        .args(["enable", &service])
        .output();

    // Do not `kickstart` here — that would immediately spawn a second ZnonClip
    // while the user is already in the UI. RunAtLoad applies at next login /
    // next bootstrap after a clean session load.
    info!("LaunchAgent ready for next login: {service}");
    Ok(())
}

fn disable_launch_agent() -> Result<(), String> {
    let domain = format!("gui/{}", uid());
    let service = format!("{domain}/{AGENT_LABEL}");

    let bootout = Command::new("launchctl")
        .args(["bootout", &service])
        .output()
        .map_err(|e| format!("launchctl bootout: {e}"))?;

    if !bootout.status.success() {
        let stderr = String::from_utf8_lossy(&bootout.stderr);
        if !stderr.is_empty()
            && !stderr.contains("No such process")
            && !stderr.contains("Could not find service")
            && !stderr.contains("not found")
            && !stderr.contains("No such file")
        {
            warn!("launchctl bootout: {}", stderr.trim());
        }
    }

    if let Ok(path) = agent_plist_path() {
        if path.is_file() {
            fs::remove_file(&path).map_err(|e| format!("remove LaunchAgent plist: {e}"))?;
            info!("removed LaunchAgent {}", path.display());
        }
    }
    Ok(())
}

trait IfEmpty {
    fn if_empty(self, alt: &str) -> String;
}

impl IfEmpty for &str {
    fn if_empty(self, alt: &str) -> String {
        if self.is_empty() {
            alt.to_string()
        } else {
            self.to_string()
        }
    }
}
