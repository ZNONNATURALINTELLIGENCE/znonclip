//! Auto-paste via synthesized `Cmd+V` keystrokes (`CGEvent`).
//!
//! Flow after the user selects a history item:
//! 1. Content is copied to the pasteboard.
//! 2. Popover closes so the previous app regains focus.
//! 3. After a short delay, we post Cmd+V key down/up.
//!
//! Requires Accessibility permission (`AXIsProcessTrusted`).

use log::{info, warn};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
};

use crate::accessibility;

/// Virtual key code for ANSI `V` (`kVK_ANSI_V`).
const KEYCODE_V: u16 = 0x09;

/// Simulate Cmd+V in the frontmost application.
///
/// Returns `Ok(())` if events were posted, `Err` if Accessibility is missing
/// or event creation failed.
pub fn paste_cmd_v() -> Result<(), AutoPasteError> {
    if !accessibility::is_process_trusted() {
        return Err(AutoPasteError::NotTrusted);
    }

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .ok_or(AutoPasteError::EventCreate)?;

    let key_down = CGEvent::new_keyboard_event(Some(&source), KEYCODE_V, true)
        .ok_or(AutoPasteError::EventCreate)?;
    let key_up = CGEvent::new_keyboard_event(Some(&source), KEYCODE_V, false)
        .ok_or(AutoPasteError::EventCreate)?;

    CGEvent::set_flags(Some(&key_down), CGEventFlags::MaskCommand);
    CGEvent::set_flags(Some(&key_up), CGEventFlags::MaskCommand);

    // Post to the HID tap so the frontmost app receives the keystroke.
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&key_down));
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&key_up));

    info!("auto-paste: posted ⌘V");
    Ok(())
}

/// Attempt auto-paste; on missing permission, prompt the user (once per call).
pub fn paste_cmd_v_or_prompt() -> Result<(), AutoPasteError> {
    match paste_cmd_v() {
        Ok(()) => Ok(()),
        Err(AutoPasteError::NotTrusted) => {
            let _ = accessibility::ensure_trusted_prompting();
            // Try once more in case trust was already granted mid-call (rare).
            if accessibility::is_process_trusted() {
                paste_cmd_v()
            } else {
                Err(AutoPasteError::NotTrusted)
            }
        }
        Err(e) => {
            warn!("auto-paste failed: {e}");
            Err(e)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoPasteError {
    NotTrusted,
    EventCreate,
}

impl std::fmt::Display for AutoPasteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTrusted => write!(
                f,
                "Accessibility permission required (System Settings → Privacy → Accessibility)"
            ),
            Self::EventCreate => write!(f, "failed to create keyboard event"),
        }
    }
}

impl std::error::Error for AutoPasteError {}
