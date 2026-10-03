//! Global hotkey registration (configurable presets, default **Cmd+Shift+V**).
//!
//! Uses `NSEvent` global + local monitors. Global monitors observe only;
//! local monitors can swallow the keystroke when Clip Assistant is key.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use block2::RcBlock;
use log::{info, warn};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags};

use crate::settings::HotkeyPreset;

/// Active hotkey key code (updated when user changes preset).
static ACTIVE_KEY_CODE: AtomicU16 = AtomicU16::new(0x09);
/// Active modifier flags (device-independent bits as u64).
static ACTIVE_MODIFIERS: AtomicU64 = AtomicU64::new(
    (NSEventModifierFlags::Command.bits() | NSEventModifierFlags::Shift.bits()) as u64,
);

/// Holds strong refs to event monitors so they stay registered.
pub struct HotkeyManager {
    global_monitor: Option<Retained<AnyObject>>,
    local_monitor: Option<Retained<AnyObject>>,
    _global_block: Option<RcBlock<dyn Fn(NonNull<NSEvent>)>>,
    _local_block: Option<RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>>,
    #[allow(dead_code)]
    preset: HotkeyPreset,
}

impl HotkeyManager {
    /// Register monitors for the given preset.
    pub fn register(preset: HotkeyPreset, _mtm: MainThreadMarker) -> Self {
        apply_preset(preset);

        let global_block = RcBlock::new(|event: NonNull<NSEvent>| {
            // SAFETY: NSEvent pointer from AppKit is valid for the call.
            let event = unsafe { event.as_ref() };
            if is_active_hotkey(event) {
                dispatch_hotkey_to_delegate();
            }
        });

        let local_block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            let event_ref = unsafe { event.as_ref() };
            if is_active_hotkey(event_ref) {
                dispatch_hotkey_to_delegate();
                return std::ptr::null_mut();
            }
            // Popover keyboard navigation (arrows / Enter / Esc), only when no
            // command-style modifier is held so ⌘A, ⌘C etc. keep working.
            if is_nav_key(event_ref) && dispatch_nav_key_to_delegate(event_ref.keyCode()) {
                return std::ptr::null_mut();
            }
            event.as_ptr()
        });

        let mask = NSEventMask::KeyDown;

        let global_monitor =
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &global_block);
        if global_monitor.is_none() {
            warn!("failed to register global hotkey monitor");
        } else {
            info!("global hotkey registered: {}", preset.display());
        }

        // SAFETY: block returns valid NSEvent* or null.
        let local_monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &local_block)
        };
        if local_monitor.is_none() {
            warn!("failed to register local hotkey monitor");
        }

        Self {
            global_monitor,
            local_monitor,
            _global_block: Some(global_block),
            _local_block: Some(local_block),
            preset,
        }
    }

    /// Drop current monitors and re-register with a new preset.
    pub fn rebind(self, preset: HotkeyPreset, mtm: MainThreadMarker) -> Self {
        drop(self);
        Self::register(preset, mtm)
    }
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        if let Some(ref mon) = self.global_monitor.take() {
            unsafe { NSEvent::removeMonitor(mon) };
        }
        if let Some(ref mon) = self.local_monitor.take() {
            unsafe { NSEvent::removeMonitor(mon) };
        }
    }
}

fn apply_preset(preset: HotkeyPreset) {
    ACTIVE_KEY_CODE.store(preset.key_code(), Ordering::Relaxed);
    let mods = match preset {
        HotkeyPreset::CmdShiftV | HotkeyPreset::CmdShiftC => {
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift
        }
        HotkeyPreset::CmdOptionV => NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        HotkeyPreset::CtrlShiftV => NSEventModifierFlags::Control | NSEventModifierFlags::Shift,
    };
    ACTIVE_MODIFIERS.store(mods.bits() as u64, Ordering::Relaxed);
}

fn is_active_hotkey(event: &NSEvent) -> bool {
    let want_key = ACTIVE_KEY_CODE.load(Ordering::Relaxed);
    if event.keyCode() != want_key {
        return false;
    }
    let want_mods = ACTIVE_MODIFIERS.load(Ordering::Relaxed);
    let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
    flags.bits() as u64 == want_mods
}

/// Key codes the popover handles itself.
pub const KEY_RETURN: u16 = 36;
pub const KEY_KEYPAD_ENTER: u16 = 76;
pub const KEY_ESCAPE: u16 = 53;
pub const KEY_DOWN: u16 = 125;
pub const KEY_UP: u16 = 126;

fn is_nav_key(event: &NSEvent) -> bool {
    let code = event.keyCode();
    if !matches!(code, KEY_RETURN | KEY_KEYPAD_ENTER | KEY_ESCAPE | KEY_DOWN | KEY_UP) {
        return false;
    }
    let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
    !flags.intersects(
        NSEventModifierFlags::Command | NSEventModifierFlags::Control | NSEventModifierFlags::Option,
    )
}

/// Ask the delegate to handle a navigation key. Returns true if it consumed it.
fn dispatch_nav_key_to_delegate(code: u16) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        return false;
    };
    // SAFETY: ClipAssistantAppDelegate implements popoverNavKey: (NSInteger) -> BOOL.
    unsafe { msg_send![&*delegate, popoverNavKey: code as isize] }
}

fn dispatch_hotkey_to_delegate() {
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("hotkey fired off main thread — ignoring");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        return;
    };
    // SAFETY: ClipAssistantAppDelegate implements hotkeyTogglePopover:.
    let _: () = unsafe {
        msg_send![&*delegate, hotkeyTogglePopover: Option::<&AnyObject>::None]
    };
}
