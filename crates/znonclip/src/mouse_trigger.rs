//! Option + right-click (or Option + two-finger click) anywhere opens the
//! floater at the pointer.
//!
//! Two mechanisms, best first:
//!
//! 1. **Event tap** (needs Accessibility, which auto-paste already asks for):
//!    sees the click before the app under the pointer does and *swallows* it,
//!    so that app's own context menu does not also appear.
//! 2. **Global + local NSEvent monitors** (no permission): open the floater
//!    too, but cannot swallow the click, so the app's menu may appear as well.
//!
//! A plain right-click is never touched. The tap is retried each time the
//! floater opens, so granting Accessibility later upgrades without a restart.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use log::{info, warn};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags};

type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventType = u32;
type CGEventMask = u64;
type CGEventFlags = u64;

type TapCallback =
    extern "C" fn(CGEventTapProxy, CGEventType, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    static kCFRunLoopCommonModes: CFStringRef;
}

const K_CG_SESSION_EVENT_TAP: u32 = 1;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: CGEventType = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: CGEventType = 0xFFFF_FFFF;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: CGEventFlags = 0x0008_0000;
const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x0010_0000;
const K_CG_EVENT_FLAG_MASK_CONTROL: CGEventFlags = 0x0004_0000;

static TAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// When the tap swallowed a right-mouse-down (ms since epoch; 0 = none). Its
/// matching up is swallowed only within `SWALLOW_WINDOW_MS`, so an up that
/// never arrived cannot make a later, ordinary right-click lose its up.
static SWALLOWED_DOWN_AT: AtomicU64 = AtomicU64::new(0);
const SWALLOW_WINDOW_MS: u64 = 1500;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Keeps the fallback monitors alive.
pub struct MouseTrigger {
    _global: Option<Retained<AnyObject>>,
    _local: Option<Retained<AnyObject>>,
    _global_block: RcBlock<dyn Fn(NonNull<NSEvent>)>,
    _local_block: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>,
}

impl MouseTrigger {
    pub fn install(_mtm: MainThreadMarker) -> Self {
        ensure_tap();

        // Fallback for other apps' windows. Skipped when the tap is live: the
        // tap already handled (and swallowed) the click.
        let global_block = RcBlock::new(|event: NonNull<NSEvent>| {
            if tap_active() {
                return;
            }
            // SAFETY: AppKit passes a valid event for the duration of the callback.
            if is_option_only(unsafe { event.as_ref() }) {
                open_at_cursor();
            }
        });
        let global = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::RightMouseDown,
            &global_block,
        );

        // Our own windows (global monitors never see them). Swallow here: we own them.
        let local_block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: as above.
            if is_option_only(unsafe { event.as_ref() }) {
                open_at_cursor();
                return ptr::null_mut();
            }
            event.as_ptr()
        });
        // SAFETY: the handler returns the event or null, as the API requires.
        let local = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::RightMouseDown,
                &local_block,
            )
        };

        info!(
            "option+right-click trigger: {}",
            if tap_active() { "event tap (click swallowed)" } else { "monitor fallback (no Accessibility yet)" }
        );
        Self {
            _global: global,
            _local: local,
            _global_block: global_block,
            _local_block: local_block,
        }
    }
}

/// Create the event tap if it does not exist yet. Fails quietly without
/// Accessibility permission; call again later to retry.
pub fn ensure_tap() {
    if tap_active() {
        return;
    }
    let mask: CGEventMask = (1 << K_CG_EVENT_RIGHT_MOUSE_DOWN) | (1 << K_CG_EVENT_RIGHT_MOUSE_UP);
    // SAFETY: plain C calls; the callback is a static extern "C" fn and the
    // run-loop source is added to the main run loop, where the callback runs.
    unsafe {
        let tap = CGEventTapCreate(
            K_CG_SESSION_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_DEFAULT,
            mask,
            tap_callback,
            ptr::null_mut(),
        );
        if tap.is_null() {
            return;
        }
        let source = CFMachPortCreateRunLoopSource(ptr::null(), tap, 0);
        if source.is_null() {
            warn!("event tap created but no run-loop source");
            return;
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        TAP.store(tap, Ordering::SeqCst);
    }
    info!("option+right-click event tap installed");
}

pub fn tap_active() -> bool {
    !TAP.load(Ordering::SeqCst).is_null()
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user: *mut c_void,
) -> CGEventRef {
    match event_type {
        K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT | K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT => {
            let tap = TAP.load(Ordering::SeqCst);
            if !tap.is_null() {
                // SAFETY: re-enabling our own tap.
                unsafe { CGEventTapEnable(tap, true) };
            }
            event
        }
        K_CG_EVENT_RIGHT_MOUSE_DOWN => {
            // SAFETY: event is valid for the callback.
            let flags = unsafe { CGEventGetFlags(event) };
            let option = flags & K_CG_EVENT_FLAG_MASK_ALTERNATE != 0;
            let other = flags & (K_CG_EVENT_FLAG_MASK_COMMAND | K_CG_EVENT_FLAG_MASK_CONTROL) != 0;
            if option && !other {
                SWALLOWED_DOWN_AT.store(now_ms(), Ordering::SeqCst);
                open_at_cursor();
                return ptr::null_mut();
            }
            event
        }
        K_CG_EVENT_RIGHT_MOUSE_UP => {
            let down = SWALLOWED_DOWN_AT.swap(0, Ordering::SeqCst);
            if down != 0 && now_ms().saturating_sub(down) <= SWALLOW_WINDOW_MS {
                ptr::null_mut()
            } else {
                event
            }
        }
        _ => event,
    }
}

fn is_option_only(event: &NSEvent) -> bool {
    let f = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
    f.contains(NSEventModifierFlags::Option)
        && !f.intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control)
}

fn open_at_cursor() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        return;
    };
    // SAFETY: ZnonClipAppDelegate implements openFloaterAtCursor:.
    let _: () = unsafe { msg_send![&*delegate, openFloaterAtCursor: ptr::null::<AnyObject>()] };
}
