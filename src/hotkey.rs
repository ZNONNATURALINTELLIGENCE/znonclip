//! Global hotkey (configurable presets, default **Cmd+Shift+V**) and popover keys.
//!
//! The global hotkey uses Carbon `RegisterEventHotKey`. Unlike an `NSEvent`
//! global key monitor (which upstream ClipPin used), it needs **no**
//! Accessibility or Input Monitoring permission, so the menu opens from the
//! keyboard on a fresh install. The hotkey is consumed: the frontmost app does
//! not also receive it.
//!
//! A separate `NSEvent` *local* monitor (no permission needed either) handles
//! ↑ / ↓ / Enter / Esc while our popover has focus.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use log::{info, warn};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags};

use crate::settings::HotkeyPreset;

// ── Carbon FFI (HIToolbox) ──────────────────────────────────────────────────

type OSStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUPP =
    extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
}

/// `kEventClassKeyboard` ('keyb').
const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
/// `kEventHotKeyPressed`.
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
/// Carbon modifier masks (`Events.h`).
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;
/// Hotkey signature ('CLAS').
const HOTKEY_SIGNATURE: u32 = u32::from_be_bytes(*b"CLAS");

/// The Carbon handler is installed once per process.
static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn hotkey_pressed(_: EventHandlerCallRef, _: EventRef, _: *mut c_void) -> OSStatus {
    dispatch_hotkey_to_delegate();
    0 // noErr
}

fn carbon_modifiers(preset: HotkeyPreset) -> u32 {
    match preset {
        HotkeyPreset::CmdShiftV | HotkeyPreset::CmdShiftC => CMD_KEY | SHIFT_KEY,
        HotkeyPreset::CmdOptionV => CMD_KEY | OPTION_KEY,
        HotkeyPreset::CtrlShiftV => CONTROL_KEY | SHIFT_KEY,
    }
}

// ── Manager ────────────────────────────────────────────────────────────────

/// Owns the Carbon hotkey registration and the popover-key local monitor.
pub struct HotkeyManager {
    hot_key: EventHotKeyRef,
    local_monitor: Option<Retained<AnyObject>>,
    _local_block: Option<RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>>,
}

impl HotkeyManager {
    /// Register the global hotkey for `preset` plus the popover-key monitor.
    pub fn register(preset: HotkeyPreset, _mtm: MainThreadMarker) -> Self {
        // SAFETY: Carbon calls on the main thread; spec array outlives the call.
        unsafe {
            if !HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
                let spec = EventTypeSpec {
                    event_class: K_EVENT_CLASS_KEYBOARD,
                    event_kind: K_EVENT_HOT_KEY_PRESSED,
                };
                let status = InstallEventHandler(
                    GetApplicationEventTarget(),
                    hotkey_pressed,
                    1,
                    &spec,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if status != 0 {
                    warn!("InstallEventHandler failed: OSStatus {status}");
                    HANDLER_INSTALLED.store(false, Ordering::SeqCst);
                }
            }
        }

        let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
        // SAFETY: valid out-pointer; target is the application event target.
        let status = unsafe {
            RegisterEventHotKey(
                preset.key_code() as u32,
                carbon_modifiers(preset),
                EventHotKeyID {
                    signature: HOTKEY_SIGNATURE,
                    id: 1,
                },
                GetApplicationEventTarget(),
                0,
                &mut hot_key,
            )
        };
        if status == 0 {
            info!("global hotkey registered: {} (Carbon, no permission needed)", preset.display());
        } else {
            // -9878 = eventHotKeyExistsErr: another app owns this combination.
            warn!(
                "RegisterEventHotKey {} failed: OSStatus {status}{}",
                preset.display(),
                if status == -9878 { " (combination taken by another app)" } else { "" }
            );
            hot_key = std::ptr::null_mut();
        }

        let local_block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            let event_ref = unsafe { event.as_ref() };
            // Popover keyboard navigation (arrows / Enter / Esc), only when no
            // command-style modifier is held so ⌘A, ⌘C etc. keep working.
            if is_nav_key(event_ref) && dispatch_nav_key_to_delegate(event_ref.keyCode()) {
                return std::ptr::null_mut();
            }
            event.as_ptr()
        });
        // SAFETY: block returns valid NSEvent* or null.
        let local_monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &local_block)
        };
        if local_monitor.is_none() {
            warn!("failed to register popover key monitor");
        }

        Self {
            hot_key,
            local_monitor,
            _local_block: Some(local_block),
        }
    }

    /// Drop current registrations and re-register with a new preset.
    pub fn rebind(self, preset: HotkeyPreset, mtm: MainThreadMarker) -> Self {
        drop(self);
        Self::register(preset, mtm)
    }
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        if !self.hot_key.is_null() {
            // SAFETY: ref came from RegisterEventHotKey and is unregistered once.
            unsafe { UnregisterEventHotKey(self.hot_key) };
            self.hot_key = std::ptr::null_mut();
        }
        if let Some(ref mon) = self.local_monitor.take() {
            unsafe { NSEvent::removeMonitor(mon) };
        }
    }
}

// ── Popover keys ───────────────────────────────────────────────────────────

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
