//! Accessibility permission helpers (`AXIsProcessTrusted`).
//!
//! Global event taps and synthesized keystrokes (auto-paste) require the user
//! to enable Clip Assistant under **System Settings → Privacy & Security → Accessibility**.

use log::{info, warn};
use std::ffi::c_void;
use std::process::Command;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;

    /// When `options` contains `AXTrustedCheckOptionPrompt = true`, macOS may
    /// show the system prompt / open the Accessibility settings pane.
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

/// Returns whether this process is trusted for Accessibility APIs.
pub fn is_process_trusted() -> bool {
    // SAFETY: Apple public C API; no pointer arguments.
    unsafe { AXIsProcessTrusted() }
}

/// Check trust; if untrusted, request the system prompt (best-effort).
///
/// Returns the trust state **after** the call (often still `false` until the
/// user toggles the switch and the app is restarted).
pub fn ensure_trusted_prompting() -> bool {
    if is_process_trusted() {
        return true;
    }

    info!("Accessibility not trusted — requesting system prompt");

    // Build a minimal CFDictionary via CoreFoundation C API for the prompt flag.
    // Using raw CF avoids pulling in extra high-level CF crates.
    let trusted = unsafe { ax_prompt_with_options() };
    if !trusted {
        // Fallback: open the Accessibility pane in System Settings (macOS 13+).
        open_accessibility_settings();
        warn!(
            "Accessibility permission required for auto-paste. \
             Enable Clip Assistant in System Settings → Privacy & Security → Accessibility, \
             then restart Clip Assistant."
        );
    }
    trusted
}

/// Open System Settings to the Accessibility privacy list (best-effort).
pub fn open_accessibility_settings() {
    // Ventura / Sonoma / Sequoia deep link (may vary slightly by OS).
    let urls = [
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
    ];
    for url in urls {
        let status = Command::new("open").arg(url).status();
        if matches!(status, Ok(s) if s.success()) {
            info!("opened Accessibility settings via {url}");
            return;
        }
    }
    warn!("could not open Accessibility settings URL");
}

/// Create `{ AXTrustedCheckOptionPrompt: true }` and call AXIsProcessTrustedWithOptions.
unsafe fn ax_prompt_with_options() -> bool {
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFBooleanTrue: *const c_void;

        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> *const c_void;

        fn CFDictionaryCreate(
            alloc: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;

        fn CFRelease(cf: *const c_void);
    }

    // kCFStringEncodingUTF8 = 0x08000100
    const UTF8: u32 = 0x0800_0100;
    let key_name = b"AXTrustedCheckOptionPrompt\0";
    let key = CFStringCreateWithCString(std::ptr::null(), key_name.as_ptr().cast(), UTF8);
    if key.is_null() {
        return AXIsProcessTrustedWithOptions(std::ptr::null());
    }

    let keys = [key];
    let values = [kCFBooleanTrue];
    let dict = CFDictionaryCreate(
        std::ptr::null(),
        keys.as_ptr(),
        values.as_ptr(),
        1,
        &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks,
    );

    let result = if dict.is_null() {
        AXIsProcessTrustedWithOptions(std::ptr::null())
    } else {
        let r = AXIsProcessTrustedWithOptions(dict);
        CFRelease(dict);
        r
    };
    CFRelease(key);
    result
}
