//! Application lifecycle: NSApplication setup and AppDelegate.
//!
//! Menu bar shell, SQLite, polling, search/pin/copy, global hotkey, auto-paste.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use log::{error, info, warn};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSEvent, NSEventType, NSRunningApplication, NSSearchField, NSStatusItem,
    NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSTimer};

use crate::accessibility;
use crate::autopaste;
use crate::clipboard::{
    copy_item_to_pasteboard, ClipboardPoller, History, PollResult, DEFAULT_HISTORY_LIMIT,
};
use crate::hotkey::{self, HotkeyManager};
use crate::launch;
use crate::mouse_trigger::{self, MouseTrigger};
use crate::preview;
use crate::predict::{self, HeuristicRanker, PasteContext, Ranker};
use crate::privacy;
use crate::settings::{Settings, MAX_PINNED};
use crate::status_item::{sender_item_id, Flash, StatusItemController};
use crate::storage::{Storage, DEFAULT_CACHE_LIMIT};

/// Set to a directory to render the floater and previews to PNG and exit.
pub const SNAPSHOT_ENV: &str = "ZNONCLIP_SNAPSHOT";

/// Delay before posting ⌘V so the previous app can regain focus after popover close.
const AUTO_PASTE_DELAY_SECS: f64 = 0.15;

/// Instance state for the Objective-C `ZnonClipAppDelegate` class.
pub struct AppDelegateIvars {
    status: RefCell<Option<StatusItemController>>,
    history: Rc<RefCell<History>>,
    poller: RefCell<ClipboardPoller>,
    storage: RefCell<Option<Storage>>,
    settings: RefCell<Settings>,
    /// Keep the timer alive for the app lifetime.
    timer: RefCell<Option<Retained<NSTimer>>>,
    /// Keep hotkey monitors alive.
    hotkey: RefCell<Option<HotkeyManager>>,
    /// One-shot timer for delayed auto-paste.
    paste_timer: RefCell<Option<Retained<NSTimer>>>,
    /// App that was frontmost when the popover opened: the paste destination.
    target_app: RefCell<Option<Retained<NSRunningApplication>>>,
    /// Hash of what we believe is on the system pasteboard right now.
    clipboard_hash: RefCell<Option<String>>,
    /// Advisory ranker for the pre-highlighted row.
    ranker: HeuristicRanker,
    /// Option + right-click anywhere opens the floater (keeps monitors alive).
    mouse_trigger: RefCell<Option<MouseTrigger>>,
    _status_item_keepalive: Cell<Option<Retained<NSStatusItem>>>,
}

impl Default for AppDelegateIvars {
    fn default() -> Self {
        Self {
            status: RefCell::new(None),
            history: Rc::new(RefCell::new(History::new(DEFAULT_HISTORY_LIMIT))),
            poller: RefCell::new(ClipboardPoller::new()),
            storage: RefCell::new(None),
            settings: RefCell::new(Settings::default()),
            timer: RefCell::new(None),
            hotkey: RefCell::new(None),
            paste_timer: RefCell::new(None),
            target_app: RefCell::new(None),
            clipboard_hash: RefCell::new(None),
            ranker: HeuristicRanker::default(),
            mouse_trigger: RefCell::new(None),
            _status_item_keepalive: Cell::new(None),
        }
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements beyond normal NSObject rules.
    // - ZnonClipAppDelegate does not implement Drop (hotkey cleaned via Option drop if we don't forget).
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "ZnonClipAppDelegate"]
    #[ivars = AppDelegateIvars]
    pub struct ZnonClipAppDelegate;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for ZnonClipAppDelegate {}

    // SAFETY: NSWindowDelegate has no additional requirements.
    unsafe impl NSWindowDelegate for ZnonClipAppDelegate {
        /// The floater was resized: re-lay out the rows to the new width.
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            self.reload_list(mtm, unsafe { &*target });
        }
    }

    // SAFETY: NSApplicationDelegate has no additional requirements.
    unsafe impl NSApplicationDelegate for ZnonClipAppDelegate {
        // SAFETY: Signature matches applicationDidFinishLaunching:.
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            info!("applicationDidFinishLaunching");

            let app = NSApplication::sharedApplication(mtm);
            let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

            match Storage::open_default() {
                Ok(storage) => {
                    let settings = Settings::load(&storage);
                    if let Err(e) = storage.prune(settings.retention_days, settings.history_limit) {
                        warn!("prune on launch failed: {e}");
                    }
                    match storage.recent_items(DEFAULT_CACHE_LIMIT) {
                        Ok(items) => {
                            info!("loaded {} items from SQLite", items.len());
                            self.ivars().history.borrow_mut().replace_all(items);
                        }
                        Err(e) => error!("failed to load history: {e}"),
                    }
                    // Sync launch-at-login UI with live SMAppService status when possible.
                    let mut settings = settings;
                    let login_st = launch::status();
                    if login_st.is_enabled() {
                        settings.launch_at_login = true;
                    }
                    *self.ivars().settings.borrow_mut() = settings;
                    *self.ivars().storage.borrow_mut() = Some(storage);
                }
                Err(e) => {
                    error!("failed to open storage — running in-memory only: {e}");
                }
            }

            let controller = StatusItemController::create(mtm);
            controller.apply_settings_ui(&self.ivars().settings.borrow());

            // SAFETY: self lives for process lifetime.
            let target: *const AnyObject = (self as *const Self).cast();
            unsafe {
                if let Some(button) = controller.status_item.button(mtm) {
                    button.setTarget(Some(&*target));
                    button.setAction(Some(sel!(togglePopover:)));
                }
                controller.set_action_target(&*target);
                controller
                    .panel
                    .setDelegate(Some(ProtocolObject::from_ref(self)));
            }

            controller.refresh_history(mtm, &self.ivars().history.borrow(), unsafe {
                &*target
            });

            if !accessibility::is_process_trusted() {
                controller.set_status_notice(Some(
                    "Auto-paste needs Accessibility permission (enable in System Settings)",
                ));
            }

            *self.ivars().status.borrow_mut() = Some(controller);

            if let Some(dir) = std::env::var_os(SNAPSHOT_ENV) {
                self.write_snapshots(std::path::Path::new(&dir));
                std::process::exit(0);
            }

            let hotkey = self.ivars().settings.borrow().hotkey;
            *self.ivars().hotkey.borrow_mut() = Some(HotkeyManager::register(hotkey, mtm));

            *self.ivars().mouse_trigger.borrow_mut() = Some(MouseTrigger::install(mtm));

            self.restart_poll_timer();

            let s = self.ivars().settings.borrow().clone();
            info!(
                "ZnonClip ready — hotkey {}, poll {}ms, auto_paste={}",
                s.hotkey.display(),
                s.poll_interval_ms,
                s.auto_paste
            );
        }
    }

    impl ZnonClipAppDelegate {
        // SAFETY: IBAction-style (sender: id).
        #[unsafe(method(togglePopover:))]
        fn toggle_popover(&self, _sender: Option<&AnyObject>) {
            let app = NSApplication::sharedApplication(self.mtm());
            let via = match app.currentEvent().map(|e| e.r#type()) {
                Some(NSEventType::RightMouseUp) => "status item right/two-finger click",
                Some(NSEventType::LeftMouseUp) => "status item left click",
                _ => "status item",
            };
            self.toggle_popover_impl(via);
        }

        /// Arrow / Enter / Esc while the popover is open. Returns true if consumed.
        // SAFETY: called from the local key monitor with an NSInteger key code.
        #[unsafe(method(popoverNavKey:))]
        fn popover_nav_key(&self, code: isize) -> Bool {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            let status_ref = self.ivars().status.borrow();
            let Some(status) = status_ref.as_ref() else {
                return Bool::NO;
            };
            if !status.is_shown() || status.is_select_mode() || status.settings_visible() {
                return Bool::NO;
            }
            // ⌘-chords on the highlighted row: ⌘P pin, ⌘E expand, ⌘⌫ delete.
            if code & hotkey::CMD_CHORD != 0 {
                let Some(id) = status.highlighted_id() else {
                    return Bool::NO;
                };
                drop(status_ref);
                match (code & !hotkey::CMD_CHORD) as u16 {
                    hotkey::KEY_P => self.toggle_pin(id),
                    hotkey::KEY_E => self.toggle_expand(id),
                    hotkey::KEY_DELETE => self.delete_animated(id),
                    _ => return Bool::NO,
                }
                return Bool::YES;
            }
            match code as u16 {
                hotkey::KEY_DOWN | hotkey::KEY_UP => {
                    let delta = if code as u16 == hotkey::KEY_DOWN { 1 } else { -1 };
                    if let Some(id) = status.move_highlight(delta) {
                        drop(status_ref);
                        self.reload_list(mtm, unsafe { &*target });
                        if preview::is_expanded() {
                            self.follow_expanded(id);
                        }
                    }
                    Bool::YES
                }
                hotkey::KEY_RETURN | hotkey::KEY_KEYPAD_ENTER => {
                    let Some(id) = status.highlighted_id() else {
                        return Bool::NO;
                    };
                    drop(status_ref);
                    info!("enter → paste item id={id}");
                    self.paste_item(id);
                    Bool::YES
                }
                hotkey::KEY_ESCAPE => {
                    drop(status_ref);
                    if preview::is_expanded() {
                        preview::close();
                    } else {
                        info!("floater close via Esc");
                        self.hide_floater();
                    }
                    Bool::YES
                }
                _ => Bool::NO,
            }
        }

        /// Invoked by global/local hotkey monitors.
        // SAFETY: same as togglePopover:.
        #[unsafe(method(hotkeyTogglePopover:))]
        fn hotkey_toggle_popover(&self, _sender: Option<&AnyObject>) {
            self.toggle_popover_impl("hotkey");
        }

        /// "Float on top" checkbox.
        // SAFETY: control action.
        #[unsafe(method(toggleFloat:))]
        fn toggle_float(&self, _sender: Option<&AnyObject>) {
            let on = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .is_some_and(|s| s.is_float_checked());
            self.ivars().settings.borrow_mut().float_panel = on;
            self.persist_settings();
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_float_on_top(on);
                status.set_status_notice(Some(if on {
                    "Float on top: stays open after paste"
                } else {
                    "Float off: closes after paste or an outside click"
                }));
            }
            info!("float_on_top → {on}");
        }

        /// ⌃⌘U (unlock = 1) / ⌃⌘L (lock = 0). Registered only while the floater
        /// is visible. P0 maps unlock/lock to the multi-select mode; the full
        /// Samsung pin-mode semantics (jitter, add/remove from pins) land in P3.
        // SAFETY: called from the Carbon hotkey handler with an NSInteger.
        #[unsafe(method(hotkeyPinMode:))]
        fn hotkey_pin_mode(&self, unlock: isize) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            let unlock = unlock != 0;
            info!("pin mode → {} via hotkey", if unlock { "unlocked" } else { "locked" });
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.show_history();
                status.set_select_mode(unlock);
                status.set_status_notice(Some(if unlock {
                    "Unlocked: select items (⌃⌘L to lock)"
                } else {
                    "Locked"
                }));
            }
            self.reload_list(mtm, unsafe { &*target });
        }

        /// Gear button — show/hide settings panel.
        // SAFETY: control action.
        #[unsafe(method(toggleSettings:))]
        fn toggle_settings(&self, _sender: Option<&AnyObject>) {
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.toggle_settings_panel();
            }
        }

        /// Poll interval popup changed.
        // SAFETY: control action.
        #[unsafe(method(pollIntervalChanged:))]
        fn poll_interval_changed(&self, _sender: Option<&AnyObject>) {
            let ms = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.selected_poll_ms())
                .unwrap_or(500)
                .max(250);
            self.ivars().settings.borrow_mut().poll_interval_ms = ms;
            self.persist_settings();
            self.restart_poll_timer();
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_status_notice(Some(&format!("Poll interval: {ms} ms")));
            }
            info!("poll interval → {ms}ms");
        }

        /// Retention days popup changed.
        // SAFETY: control action.
        #[unsafe(method(retentionChanged:))]
        fn retention_changed(&self, _sender: Option<&AnyObject>) {
            let days = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.selected_retention_days())
                .unwrap_or(30);
            self.ivars().settings.borrow_mut().retention_days = days;
            self.persist_settings();
            self.run_prune();
            if let Some(ref status) = *self.ivars().status.borrow() {
                let msg = if days == 0 {
                    "Retention: unlimited (time)".to_string()
                } else {
                    format!("Retention: {days} days")
                };
                status.set_status_notice(Some(&msg));
            }
        }

        /// History max-count popup changed.
        // SAFETY: control action.
        #[unsafe(method(historyLimitChanged:))]
        fn history_limit_changed(&self, _sender: Option<&AnyObject>) {
            let limit = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.selected_history_limit())
                .unwrap_or(1000);
            self.ivars().settings.borrow_mut().history_limit = limit;
            self.persist_settings();
            self.run_prune();
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_status_notice(Some(&format!("Max items: {limit}")));
            }
        }

        /// Hotkey preset popup changed.
        // SAFETY: control action.
        #[unsafe(method(hotkeyChanged:))]
        fn hotkey_changed(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let preset = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.selected_hotkey())
                .unwrap_or_default();
            self.ivars().settings.borrow_mut().hotkey = preset;
            self.persist_settings();

            // Rebind monitors.
            let old = self.ivars().hotkey.borrow_mut().take();
            if let Some(mgr) = old {
                let mut mgr = mgr.rebind(preset, mtm);
                // Settings are edited inside the floater, so it is visible now.
                mgr.set_pin_keys_active(true);
                *self.ivars().hotkey.borrow_mut() = Some(mgr);
            } else {
                *self.ivars().hotkey.borrow_mut() = Some(HotkeyManager::register(preset, mtm));
            }

            if let Some(ref status) = *self.ivars().status.borrow() {
                status
                    .hotkey_hint
                    .setStringValue(&objc2_foundation::NSString::from_str(preset.display()));
                status.set_status_notice(Some(&format!("Hotkey: {}", preset.display())));
            }
            info!("hotkey → {}", preset.as_str());
        }

        /// Launch at login checkbox.
        // SAFETY: control action.
        #[unsafe(method(toggleLaunchAtLogin:))]
        fn toggle_launch_at_login(&self, _sender: Option<&AnyObject>) {
            let want = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.is_launch_at_login_checked())
                .unwrap_or(false);

            match launch::set_enabled(want) {
                Ok(st) => {
                    // Reflect actual status (e.g. RequiresApproval still not fully on).
                    let enabled = st.is_enabled();
                    let checked = enabled
                        || (want && matches!(st, launch::LoginItemStatus::RequiresApproval));
                    self.ivars().settings.borrow_mut().launch_at_login = enabled || checked;
                    if let Some(ref status) = *self.ivars().status.borrow() {
                        status.set_launch_at_login_checked(checked);
                        let notice = if want
                            && matches!(st, launch::LoginItemStatus::RequiresApproval)
                        {
                            "Login item needs approval in System Settings → Login Items"
                        } else if enabled {
                            "Launch at login enabled"
                        } else if !want {
                            "Launch at login disabled"
                        } else {
                            st.describe()
                        };
                        status.set_status_notice(Some(notice));
                    }
                    self.persist_settings();
                    info!("launch_at_login want={want} status={}", st.describe());
                }
                Err(msg) => {
                    if let Some(ref status) = *self.ivars().status.borrow() {
                        status.set_launch_at_login_checked(false);
                        status.set_status_notice(Some(&msg));
                    }
                    self.ivars().settings.borrow_mut().launch_at_login = false;
                    self.persist_settings();
                }
            }
        }

        // SAFETY: timer selector.
        #[unsafe(method(pollClipboard:))]
        fn poll_clipboard(&self, _timer: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();

            match self.ivars().poller.borrow_mut().poll() {
                PollResult::NoChange | PollResult::EmptyOrUnsupported => {}
                PollResult::SkippedPrivate(markers) => {
                    if let Some(ref status) = *self.ivars().status.borrow() {
                        let msg = privacy::status_message(markers);
                        status.set_status_notice(Some(&msg));
                    }
                }
                PollResult::Captured(mut item, full) => {
                    let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
                    let recheck = privacy::inspect_pasteboard(&pb);
                    if recheck.is_sensitive() {
                        privacy::log_skipped(recheck);
                        if let Some(ref status) = *self.ivars().status.borrow() {
                            let msg = privacy::status_message(recheck);
                            status.set_status_notice(Some(&msg));
                        }
                        return;
                    }

                    if let Some(ref status) = *self.ivars().status.borrow() {
                        // Don't clear accessibility tip unless it was a privacy message.
                        status.set_status_notice(None);
                    }
                    *self.ivars().clipboard_hash.borrow_mut() = Some(item.hash.clone());

                    if let Some(ref storage) = *self.ivars().storage.borrow() {
                        match storage.touch_latest_if_hash(&item.hash) {
                            Ok(Some(existing_id)) => {
                                item.id = existing_id;
                                item.created_at = String::new();
                                self.ivars().history.borrow_mut().push_front(item);
                                self.reload_list(mtm, unsafe { &*target });
                                return;
                            }
                            Ok(None) => {
                                match storage.insert_item(&item) {
                                    Ok((id, created_at)) => {
                                        item.id = id;
                                        item.created_at = created_at;
                                        if let Some(ref full) = full {
                                            if let Err(e) = storage.set_full_image(id, full) {
                                                warn!("storing original image failed: {e}");
                                            }
                                        }
                                    }
                                    Err(e) => error!("failed to insert clipboard item: {e}"),
                                }
                                let (days, limit) = {
                                    let s = self.ivars().settings.borrow();
                                    (s.retention_days, s.history_limit)
                                };
                                match storage.prune(days, limit) {
                                    Ok(n) if n > 0 => {
                                        // Rows were pruned: reload so the in-memory
                                        // list matches the DB (new item included).
                                        if let Ok(items) = storage.recent_items(DEFAULT_CACHE_LIMIT) {
                                            self.ivars().history.borrow_mut().replace_all(items);
                                        }
                                        self.reload_list(mtm, unsafe { &*target });
                                        return;
                                    }
                                    Ok(_) => {}
                                    Err(e) => warn!("prune after insert failed: {e}"),
                                }
                            }
                            Err(e) => error!("dedup check failed: {e}"),
                        }
                    }

                    self.ivars().history.borrow_mut().push_front(item);
                    self.reload_list(mtm, unsafe { &*target });
                }
            }
        }

        /// Search field continuous action.
        // SAFETY: control action signature.
        #[unsafe(method(searchFieldChanged:))]
        fn search_field_changed(&self, sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();

            let query = sender
                .and_then(|s| s.downcast_ref::<NSSearchField>())
                .map(|f| f.stringValue().to_string())
                .unwrap_or_default();
            let q = query.trim().to_string();

            // A typed query is explicit intent: drop the suggestion while searching,
            // re-run the ranker when the query is cleared.
            if q.is_empty() {
                self.update_prediction();
            } else if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_suggestion(None);
            }

            if let Some(ref status) = *self.ivars().status.borrow() {
                if q.is_empty() {
                    status.refresh_history(mtm, &self.ivars().history.borrow(), unsafe {
                        &*target
                    });
                    return;
                }

                let items = if let Some(ref storage) = *self.ivars().storage.borrow() {
                    match storage.search_items(&q, DEFAULT_CACHE_LIMIT) {
                        Ok(rows) => rows,
                        Err(e) => {
                            warn!("search failed: {e}");
                            self.ivars().history.borrow().filter(&q)
                        }
                    }
                } else {
                    self.ivars().history.borrow().filter(&q)
                };
                status.render_items(mtm, &items, unsafe { &*target }, false);
            }
        }

        /// Auto-paste checkbox toggled.
        // SAFETY: control action.
        #[unsafe(method(toggleAutoPaste:))]
        fn toggle_auto_paste(&self, _sender: Option<&AnyObject>) {
            let enabled = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.is_auto_paste_checked())
                .unwrap_or(false);

            self.ivars().settings.borrow_mut().auto_paste = enabled;
            if let Some(ref storage) = *self.ivars().storage.borrow() {
                self.ivars().settings.borrow().save_auto_paste(storage);
            }

            if enabled && !accessibility::is_process_trusted() {
                let _ = accessibility::ensure_trusted_prompting();
                if let Some(ref status) = *self.ivars().status.borrow() {
                    status.set_status_notice(Some(
                        "Enable ZnonClip in Accessibility, then restart for auto-paste",
                    ));
                }
            } else if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_status_notice(Some(if enabled {
                    "Auto-paste on"
                } else {
                    "Auto-paste off"
                }));
            }
            info!("auto_paste set to {enabled}");
        }

        /// Click row or context-menu Copy — pasteboard write + optional auto-paste.
        // SAFETY: control/menu action.
        #[unsafe(method(copyHistoryItem:))]
        fn copy_history_item(&self, sender: Option<&AnyObject>) {
            let Some(id) = sender_item_id(sender) else {
                return;
            };
            self.paste_item(id);
        }

        /// Delayed auto-paste timer callback.
        // SAFETY: timer selector.
        #[unsafe(method(performAutoPaste:))]
        fn perform_auto_paste(&self, _timer: Option<&AnyObject>) {
            *self.ivars().paste_timer.borrow_mut() = None;
            match autopaste::paste_cmd_v_or_prompt() {
                Ok(()) => {
                    if let Some(ref status) = *self.ivars().status.borrow() {
                        status.set_status_notice(Some("Auto-pasted"));
                    }
                }
                Err(autopaste::AutoPasteError::NotTrusted) => {
                    if let Some(ref status) = *self.ivars().status.borrow() {
                        status.set_status_notice(Some(
                            "Copied — grant Accessibility to enable auto-paste",
                        ));
                    }
                }
                Err(e) => {
                    warn!("auto-paste error: {e}");
                    if let Some(ref status) = *self.ivars().status.borrow() {
                        status.set_status_notice(Some("Copied (auto-paste failed)"));
                    }
                }
            }
        }

        /// Row Pin button, context-menu Pin / Unpin, or ⌘P.
        // SAFETY: control/menu action.
        #[unsafe(method(pinHistoryItem:))]
        fn pin_history_item(&self, sender: Option<&AnyObject>) {
            if let Some(id) = sender_item_id(sender) {
                self.toggle_pin(id);
            }
        }

        /// Row Delete button or context-menu Delete: animate the row out first.
        // SAFETY: menu/control action.
        #[unsafe(method(deleteHistoryItem:))]
        fn delete_history_item(&self, sender: Option<&AnyObject>) {
            if let Some(id) = sender_item_id(sender) {
                self.delete_animated(id);
            }
        }

        /// Row Expand button or ⌘E: toggle the large preview.
        // SAFETY: control/menu action.
        #[unsafe(method(expandHistoryItem:))]
        fn expand_history_item(&self, sender: Option<&AnyObject>) {
            if let Some(id) = sender_item_id(sender) {
                self.toggle_expand(id);
            }
        }

        /// Close button on the expanded preview.
        // SAFETY: control action.
        #[unsafe(method(collapsePreview:))]
        fn collapse_preview(&self, _sender: Option<&AnyObject>) {
            preview::close();
        }

        /// Select mode: pin every selected row, or unpin them if all are pinned.
        // SAFETY: control action.
        #[unsafe(method(pinSelectedItems:))]
        fn pin_selected_items(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            let ids = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.selected_ids())
                .unwrap_or_default();
            if ids.is_empty() {
                return;
            }
            let all_pinned = {
                let h = self.ivars().history.borrow();
                ids.iter().all(|id| h.get(*id).is_some_and(|i| i.is_pinned))
            };
            let want = !all_pinned;
            let mut changed = 0usize;
            let mut refused = 0usize;
            for id in &ids {
                if want && self.pinned_count() >= MAX_PINNED {
                    refused += 1;
                    continue;
                }
                if self.set_pin(*id, want) {
                    changed += 1;
                }
            }
            self.reload_cache();
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_select_mode(false);
                let mut msg = format!("{} {changed}", if want { "Pinned" } else { "Unpinned" });
                if refused > 0 {
                    msg.push_str(&format!(" · {refused} over the {MAX_PINNED}-pin limit"));
                }
                status.set_status_notice(Some(&msg));
            }
            self.reload_list(mtm, unsafe { &*target });
        }

        /// Option + right-click anywhere: open (or move) the floater at the pointer.
        // SAFETY: called by mouse_trigger with a nil sender.
        #[unsafe(method(openFloaterAtCursor:))]
        fn open_floater_at_cursor(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            let point = NSEvent::mouseLocation();
            let opening = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .is_some_and(|s| !s.is_shown());
            info!("floater {} via option+right-click", if opening { "open" } else { "move" });
            if opening {
                self.capture_target_app();
                self.update_prediction();
            }
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.show_history();
                status.refresh_history(mtm, &self.ivars().history.borrow(), unsafe { &*target });
                status.open_at(mtm, point);
            }
            if opening {
                if let Some(ref mut hk) = *self.ivars().hotkey.borrow_mut() {
                    hk.set_pin_keys_active(true);
                }
            }
        }

        /// Enter multi-select mode.
        // SAFETY: control action.
        #[unsafe(method(toggleSelectMode:))]
        fn toggle_select_mode(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_select_mode(true);
                status.set_status_notice(Some("Select items, then Delete"));
            }
            self.reload_list(mtm, unsafe { &*target });
        }

        /// Leave multi-select mode without deleting.
        // SAFETY: control action.
        #[unsafe(method(cancelSelectMode:))]
        fn cancel_select_mode(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_select_mode(false);
                status.set_status_notice(None);
            }
            self.reload_list(mtm, unsafe { &*target });
        }

        /// Toggle selection for one row while in select mode.
        // SAFETY: control action.
        #[unsafe(method(toggleItemSelection:))]
        fn toggle_item_selection(&self, sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            let Some(id) = sender_item_id(sender) else {
                return;
            };
            if let Some(ref status) = *self.ivars().status.borrow() {
                if !status.is_select_mode() {
                    return;
                }
                let _ = status.toggle_selection(id);
                let n = status.selected_count();
                status.refresh_toolbar_mode();
                status.set_status_notice(Some(&format!(
                    "{n} selected"
                )));
            }
            self.reload_list(mtm, unsafe { &*target });
        }

        /// Delete all currently selected items.
        // SAFETY: control action.
        #[unsafe(method(deleteSelectedItems:))]
        fn delete_selected_items(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();
            let ids = self
                .ivars()
                .status
                .borrow()
                .as_ref()
                .map(|s| s.selected_ids())
                .unwrap_or_default();
            if ids.is_empty() {
                return;
            }

            if let Some(ref storage) = *self.ivars().storage.borrow() {
                match storage.delete_items(&ids) {
                    Ok(n) => info!("deleted {n} selected items"),
                    Err(e) => {
                        error!("bulk delete failed: {e}");
                        return;
                    }
                }
            }
            self.ivars().history.borrow_mut().remove_many(&ids);

            if let Some(ref status) = *self.ivars().status.borrow() {
                let n = ids.len();
                status.set_select_mode(false);
                status.set_status_notice(Some(&format!("Deleted {n} items")));
            }
            self.reload_list(mtm, unsafe { &*target });
        }

        /// Clear unpinned history (pinned favorites are kept).
        // SAFETY: control action.
        #[unsafe(method(clearHistory:))]
        fn clear_history(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let target: *const AnyObject = (self as *const Self).cast();

            if let Some(ref storage) = *self.ivars().storage.borrow() {
                match storage.clear_unpinned() {
                    Ok(n) => info!("cleared {n} unpinned items"),
                    Err(e) => {
                        error!("clear history failed: {e}");
                        return;
                    }
                }
            }
            self.ivars().history.borrow_mut().clear_unpinned();

            // Reload cache from DB so pinned rows stay consistent.
            if let Some(ref storage) = *self.ivars().storage.borrow() {
                if let Ok(items) = storage.recent_items(DEFAULT_CACHE_LIMIT) {
                    self.ivars().history.borrow_mut().replace_all(items);
                }
            }

            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_select_mode(false);
                status.set_status_notice(Some("History cleared (pinned kept)"));
            }
            self.reload_list(mtm, unsafe { &*target });
        }
    }
);

impl ZnonClipAppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        // SAFETY: NSObject init signature is correct.
        unsafe { msg_send![super(this), init] }
    }

    /// Pin or unpin `id` (respecting the pin cap), re-render, flash the row.
    fn toggle_pin(&self, id: u64) {
        let mtm = self.mtm();
        let target: *const AnyObject = (self as *const Self).cast();
        let currently_pinned = self
            .ivars()
            .history
            .borrow()
            .get(id)
            .map(|i| i.is_pinned)
            .or_else(|| {
                self.ivars()
                    .storage
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.get_item(id).ok().flatten())
                    .map(|i| i.is_pinned)
            })
            .unwrap_or(false);
        let new_pin = !currently_pinned;
        if new_pin && self.pinned_count() >= MAX_PINNED {
            if let Some(ref status) = *self.ivars().status.borrow() {
                status.set_status_notice(Some(&format!("{MAX_PINNED} pins max: unpin one first")));
            }
            info!("pin refused: cap {MAX_PINNED}");
            return;
        }
        if !self.set_pin(id, new_pin) {
            return;
        }
        self.reload_cache();
        if let Some(ref status) = *self.ivars().status.borrow() {
            status.flash_next(id, if new_pin { Flash::Pinned } else { Flash::Unpinned });
            status.set_status_notice(Some(if new_pin { "Pinned" } else { "Unpinned" }));
        }
        self.reload_list(mtm, unsafe { &*target });
        info!("item id={id} pinned={new_pin}");
    }

    /// Write one pin state to storage and the cache. False if storage refused.
    fn set_pin(&self, id: u64, pinned: bool) -> bool {
        if let Some(ref storage) = *self.ivars().storage.borrow() {
            if let Err(e) = storage.set_pinned(id, pinned) {
                error!("set_pinned failed: {e}");
                return false;
            }
        }
        self.ivars().history.borrow_mut().set_pinned(id, pinned);
        true
    }

    fn pinned_count(&self) -> usize {
        self.ivars()
            .storage
            .borrow()
            .as_ref()
            .and_then(|s| s.pinned_count().ok())
            .unwrap_or_else(|| self.ivars().history.borrow().filter("").iter().filter(|i| i.is_pinned).count())
    }

    /// Refresh the in-memory cache from SQLite (pin order, pruning).
    fn reload_cache(&self) {
        if let Some(ref storage) = *self.ivars().storage.borrow() {
            if let Ok(items) = storage.recent_items(DEFAULT_CACHE_LIMIT) {
                self.ivars().history.borrow_mut().replace_all(items);
            }
        }
    }

    /// Slide the row out, then delete it. Rows not on screen go straight away.
    fn delete_animated(&self, id: u64) {
        let row = self.ivars().status.borrow().as_ref().and_then(|s| s.row_view(id));
        match row {
            Some(row) => {
                // SAFETY: the delegate is leaked in run() and lives for the process.
                let me = self as *const Self as usize;
                row.animate_removal(Box::new(move || unsafe { &*(me as *const Self) }.commit_delete(id)));
            }
            None => self.commit_delete(id),
        }
    }

    fn commit_delete(&self, id: u64) {
        let mtm = self.mtm();
        let target: *const AnyObject = (self as *const Self).cast();
        if let Some(ref storage) = *self.ivars().storage.borrow() {
            match storage.delete_item(id) {
                Ok(true) => info!("deleted item id={id}"),
                Ok(false) => warn!("delete: no row id={id}"),
                Err(e) => {
                    error!("delete failed: {e}");
                    return;
                }
            }
        }
        self.ivars().history.borrow_mut().remove(id);
        preview::close();
        self.reload_list(mtm, unsafe { &*target });
        if let Some(ref status) = *self.ivars().status.borrow() {
            status.set_status_notice(Some("Deleted"));
        }
    }

    fn item_by_id(&self, id: u64) -> Option<crate::clipboard::ClipboardItem> {
        self.ivars().history.borrow().get(id).cloned().or_else(|| {
            self.ivars()
                .storage
                .borrow()
                .as_ref()
                .and_then(|s| s.get_item(id).ok().flatten())
        })
    }

    /// The item with its original image (when kept) in place of the thumbnail,
    /// so the expanded preview is sharp.
    fn item_for_expanded(&self, id: u64) -> Option<crate::clipboard::ClipboardItem> {
        let mut item = self.item_by_id(id)?;
        if item.content_type == crate::clipboard::ContentType::Image {
            if let Some(full) = self
                .ivars()
                .storage
                .borrow()
                .as_ref()
                .and_then(|s| s.full_image(id).ok().flatten())
            {
                item.content_image = Some(full.bytes);
            }
        }
        Some(item)
    }

    /// Anchor rect for a preview of `id`: its row if visible, else the floater.
    fn preview_anchor(&self, id: u64) -> Option<(objc2_foundation::NSRect, objc2_foundation::NSRect)> {
        let status = self.ivars().status.borrow();
        let status = status.as_ref()?;
        let floater = status.floater_frame();
        let anchor = status.row_view(id).map(|r| r.screen_rect()).unwrap_or(floater);
        Some((anchor, floater))
    }

    fn toggle_expand(&self, id: u64) {
        let Some(item) = self.item_for_expanded(id) else {
            return;
        };
        let Some((anchor, floater)) = self.preview_anchor(id) else {
            return;
        };
        let target: *const AnyObject = (self as *const Self).cast();
        preview::toggle_expanded(self.mtm(), &item, anchor, floater, unsafe { &*target });
    }

    fn follow_expanded(&self, id: u64) {
        let (Some(item), Some((anchor, floater))) = (self.item_for_expanded(id), self.preview_anchor(id)) else {
            return;
        };
        let target: *const AnyObject = (self as *const Self).cast();
        preview::follow_if_expanded(self.mtm(), &item, anchor, floater, unsafe { &*target });
    }

    /// Headless visual check: floater (normal + select mode) and both preview
    /// modes for one image and one text item.
    fn write_snapshots(&self, dir: &std::path::Path) {
        let mtm = self.mtm();
        let target: *const AnyObject = (self as *const Self).cast();
        let _ = std::fs::create_dir_all(dir);
        let save = |view: &objc2_app_kit::NSView, name: &str| {
            let path = dir.join(name);
            match crate::theme::save_png(view, &path) {
                Ok(()) => println!("wrote {}", path.display()),
                Err(e) => eprintln!("snapshot {name} failed: {e}"),
            }
        };
        let items = self.ivars().history.borrow().filter("");
        if let Some(ref status) = *self.ivars().status.borrow() {
            if let Some(first) = items.iter().find(|i| i.is_pinned).or(items.first()) {
                status.set_suggestion(Some((first.id, "pasted here before".into())));
            }
            status.refresh_history(mtm, &self.ivars().history.borrow(), unsafe { &*target });
            if let Some(content) = status.panel.contentView() {
                save(&content, "floater.png");
            }
            status.set_select_mode(true);
            for i in items.iter().take(2) {
                status.toggle_selection(i.id);
            }
            status.refresh_toolbar_mode();
            status.refresh_history(mtm, &self.ivars().history.borrow(), unsafe { &*target });
            if let Some(content) = status.panel.contentView() {
                save(&content, "floater-select.png");
            }
        }
        let image = items.iter().find(|i| i.content_type == crate::clipboard::ContentType::Image);
        let text = items
            .iter()
            .filter(|i| i.content_text.is_some())
            .max_by_key(|i| i.content_text.as_ref().map_or(0, |t| t.len()));
        for (item, tag) in [(image, "image"), (text, "text")] {
            if let Some(item) = item {
                save(&preview::snapshot_view(mtm, item, false), &format!("preview-{tag}-hover.png"));
                save(&preview::snapshot_view(mtm, item, true), &format!("preview-{tag}-expanded.png"));
            }
        }
    }

    fn toggle_popover_impl(&self, via: &str) {
        let mtm = self.mtm();
        let target: *const AnyObject = (self as *const Self).cast();
        let opening = self
            .ivars()
            .status
            .borrow()
            .as_ref()
            .is_some_and(|s| !s.is_shown());
        info!("popover {} via {via}", if opening { "open" } else { "close" });
        if opening {
            mouse_trigger::ensure_tap();
            self.capture_target_app();
            self.update_prediction();
        }
        if let Some(ref status) = *self.ivars().status.borrow() {
            status.show_history();
            status.refresh_history(mtm, &self.ivars().history.borrow(), unsafe {
                &*target
            });
            status.toggle_popover(mtm);
        }
        if let Some(ref mut hk) = *self.ivars().hotkey.borrow_mut() {
            hk.set_pin_keys_active(opening);
        }
    }

    /// Remember the frontmost app (the paste destination) before we activate.
    fn capture_target_app(&self) {
        // While we are active (e.g. reopened from our own popover) the frontmost
        // app is us: keep the previous target.
        if NSApplication::sharedApplication(self.mtm()).isActive() {
            return;
        }
        match NSWorkspace::sharedWorkspace().frontmostApplication() {
            Some(app) => {
                info!(
                    "paste target: {}",
                    app.bundleIdentifier()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "(unknown)".into())
                );
                *self.ivars().target_app.borrow_mut() = Some(app);
            }
            None => {}
        }
    }

    fn target_bundle_id(&self) -> Option<String> {
        self.ivars()
            .target_app
            .borrow()
            .as_ref()
            .and_then(|a| a.bundleIdentifier())
            .map(|s| s.to_string())
    }

    /// Run the advisory ranker and pre-highlight its pick. Never fails the open:
    /// with no storage or no items the highlight simply falls back to the top row.
    fn update_prediction(&self) {
        let target = self.target_bundle_id();
        let stats = self
            .ivars()
            .storage
            .borrow()
            .as_ref()
            .and_then(|s| s.paste_stats(target.as_deref()).ok())
            .unwrap_or_default();
        let ctx = PasteContext::new(target, self.ivars().clipboard_hash.borrow().clone(), stats);
        let items = self.ivars().history.borrow().filter("");
        let ranker = &self.ivars().ranker;
        let prediction = predict::predict(ranker, &items, &ctx);
        if let Some(ref p) = prediction {
            info!(
                "prediction [{}]: item id={} score={:.3} ({}) target={}",
                ranker.name(),
                p.item_id,
                p.score,
                p.reason,
                ctx.target_app.as_deref().unwrap_or("(unknown)")
            );
        }
        if let Some(ref status) = *self.ivars().status.borrow() {
            status.set_suggestion(prediction.map(|p| (p.item_id, p.reason.to_string())));
        }
    }

    /// Put `id` on the pasteboard, close the popover, return focus to the target
    /// app and (if enabled) synthesize ⌘V. Shared by click and Enter.
    fn paste_item(&self, id: u64) {
        let item = self
            .ivars()
            .history
            .borrow()
            .get(id)
            .cloned()
            .or_else(|| {
                self.ivars()
                    .storage
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.get_item(id).ok().flatten())
            });

        let Some(item) = item else {
            warn!("paste: item id={id} not found");
            return;
        };

        // Avoid re-ingesting our own write as a new history entry.
        self.ivars().poller.borrow_mut().ignore_next_change();
        let full = if item.content_type == crate::clipboard::ContentType::Image {
            self.ivars()
                .storage
                .borrow()
                .as_ref()
                .and_then(|s| s.full_image(id).ok().flatten())
        } else {
            None
        };
        copy_item_to_pasteboard(&item, full.as_ref());
        *self.ivars().clipboard_hash.borrow_mut() = Some(item.hash.clone());

        let target_bundle = self.target_bundle_id();
        if let Some(ref storage) = *self.ivars().storage.borrow() {
            if let Err(e) = storage.record_paste(id, target_bundle.as_deref(), item.content_type) {
                warn!("record_paste failed: {e}");
            }
        }

        // The floater never activates this app, so the target app is still the
        // active one. Floating: stay visible but hand keyboard focus back.
        // Not floating: close, like a menu.
        let float = self
            .ivars()
            .status
            .borrow()
            .as_ref()
            .is_some_and(|s| s.float_on_top());
        if float {
            if let Some(ref status) = *self.ivars().status.borrow() {
                if let Some(row) = status.row_view(id) {
                    crate::theme::flash(self.mtm(), &row, &crate::theme::neon(), 9.0);
                }
                status.release_focus();
            }
        } else {
            self.hide_floater();
        }

        // Fallback: if this app did become active (e.g. a settings menu was
        // used), hand activation back to where the user was.
        if NSApplication::sharedApplication(self.mtm()).isActive() {
            if let Some(ref app) = *self.ivars().target_app.borrow() {
                let _ = app.activateWithOptions(NSApplicationActivationOptions::empty());
            }
        }

        let auto_paste = self.ivars().settings.borrow().auto_paste;
        if auto_paste {
            self.schedule_auto_paste();
        } else if let Some(ref status) = *self.ivars().status.borrow() {
            status.set_status_notice(Some("Copied to clipboard"));
        }
        info!(
            "pasted item id={id} → {} (auto_paste={auto_paste})",
            target_bundle.as_deref().unwrap_or("(unknown)")
        );
    }

    /// Hide the floater and release ⌃⌘U / ⌃⌘L.
    fn hide_floater(&self) {
        if let Some(ref status) = *self.ivars().status.borrow() {
            status.close();
        }
        if let Some(ref mut hk) = *self.ivars().hotkey.borrow_mut() {
            hk.set_pin_keys_active(false);
        }
    }

    fn persist_settings(&self) {
        if let Some(ref storage) = *self.ivars().storage.borrow() {
            self.ivars().settings.borrow().save_all(storage);
        }
    }

    fn run_prune(&self) {
        if let Some(ref storage) = *self.ivars().storage.borrow() {
            let (days, limit) = {
                let s = self.ivars().settings.borrow();
                (s.retention_days, s.history_limit)
            };
            if let Err(e) = storage.prune(days, limit) {
                warn!("prune failed: {e}");
            }
        }
    }

    fn restart_poll_timer(&self) {
        if let Some(old) = self.ivars().timer.borrow_mut().take() {
            old.invalidate();
        }
        let secs = self.ivars().settings.borrow().poll_interval_ms as f64 / 1000.0;
        let secs = secs.max(0.25);
        // SAFETY: target is self; selector pollClipboard: is implemented.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                secs,
                &*((self as *const Self).cast::<AnyObject>()),
                sel!(pollClipboard:),
                None,
                true,
            )
        };
        *self.ivars().timer.borrow_mut() = Some(timer);
        info!("poll timer restarted at {secs:.3}s");
    }

    fn schedule_auto_paste(&self) {
        // Cancel any pending paste timer.
        if let Some(old) = self.ivars().paste_timer.borrow_mut().take() {
            old.invalidate();
        }

        // SAFETY: target is self; selector performAutoPaste: is implemented.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                AUTO_PASTE_DELAY_SECS,
                &*((self as *const Self).cast::<AnyObject>()),
                sel!(performAutoPaste:),
                None,
                false,
            )
        };
        *self.ivars().paste_timer.borrow_mut() = Some(timer);
    }

    fn reload_list(&self, mtm: MainThreadMarker, target: &AnyObject) {
        if let Some(ref status) = *self.ivars().status.borrow() {
            let q = status.search_query();
            if q.trim().is_empty() {
                status.refresh_history(mtm, &self.ivars().history.borrow(), target);
            } else if let Some(ref storage) = *self.ivars().storage.borrow() {
                match storage.search_items(q.trim(), DEFAULT_CACHE_LIMIT) {
                    Ok(items) => status.render_items(mtm, &items, target, false),
                    Err(_) => status.refresh_history(mtm, &self.ivars().history.borrow(), target),
                }
            } else {
                status.refresh_history(mtm, &self.ivars().history.borrow(), target);
            }
        }
    }
}

/// Start the NSApplication run loop (blocks until quit).
pub fn run() {
    let mtm = MainThreadMarker::new().expect("UI must run on the main thread");

    let app = NSApplication::sharedApplication(mtm);
    let delegate = ZnonClipAppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    std::mem::forget(delegate);

    info!("entering NSApplication run loop");
    app.run();
}
