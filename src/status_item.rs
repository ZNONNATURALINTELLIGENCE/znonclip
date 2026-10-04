//! Menu bar icon (`NSStatusItem`) and the floater (`NSPanel`).
//!
//! The floater is a translucent, draggable, resizable, non-activating panel:
//! it can take keyboard focus for search and arrow keys without making this app
//! the active app, so the app you were typing in stays active and receives the
//! paste. With "Float on top" it stays above other windows and remains open
//! after a paste; without it, it closes on paste or on a click elsewhere.
//!
//! Visual language: macOS menu-bar utilities (Little Snitch Mini–adjacent) —
//! system semantic colors, quiet chrome, clear hierarchy, SF Symbols, compact rows.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use log::info;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{sel, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBezelStyle, NSBox, NSBoxType, NSButton, NSCellImagePosition, NSColor, NSControl,
    NSControlStateValueOff, NSControlStateValueOn, NSEvent, NSEventMask, NSFont,
    NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSImage, NSImageScaling,
    NSImageSymbolConfiguration, NSImageSymbolScale, NSLineBreakMode, NSMenu, NSMenuItem,
    NSAutoresizingMaskOptions, NSBackingStoreType, NSFloatingWindowLevel, NSNormalWindowLevel,
    NSPanel, NSPopUpButton, NSScrollView, NSSearchField, NSStatusBar, NSStatusItem,
    NSTextAlignment, NSTextField, NSTitlePosition, NSVariableStatusItemLength, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowButton, NSWindowCollectionBehavior, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{ns_string, NSPoint, NSRect, NSSize, NSString};

use crate::clipboard::{ClipboardItem, History};
use crate::settings::{
    history_limit_label, poll_label, retention_label, HotkeyPreset, Settings,
    DEFAULT_HISTORY_ITEMS, HISTORY_LIMIT_OPTIONS, POLL_INTERVAL_OPTIONS, RETENTION_DAY_OPTIONS,
};

/// Initial floater content size (points). The user can resize it; the size and
/// position are remembered (frame autosave).
pub const POPOVER_WIDTH: f64 = 380.0;
pub const POPOVER_HEIGHT: f64 = 500.0;
/// Smallest floater the layout supports.
const MIN_W: f64 = 320.0;
const MIN_H: f64 = 260.0;
/// Frame autosave key (NSUserDefaults).
const FRAME_AUTOSAVE: &str = "ZnonClipFloater";

const HEADER_H: f64 = 30.0;
const SEARCH_H: f64 = 28.0;
const TOOLBAR_H: f64 = 22.0;
const STATUS_H: f64 = 20.0;
const PAD: f64 = 12.0;
const ROW_H: f64 = 32.0;
const SECTION_H: f64 = 18.0;
const SEP_H: f64 = 1.0;
const ROW_GAP: f64 = 2.0;
const ROW_RADIUS: f64 = 6.0;
const TIME_COL: f64 = 44.0;

/// Owns strong refs for the menu-bar UI (must not drop while app runs).
pub struct StatusItemController {
    pub status_item: Retained<NSStatusItem>,
    pub panel: Retained<NSPanel>,
    /// "Float on top" checkbox in the header.
    pub float_checkbox: Retained<NSButton>,
    /// Shared with the outside-click monitor: when true the floater stays open.
    float_on_top: Rc<Cell<bool>>,
    /// True until the floater has been placed once (first open goes under the icon).
    needs_initial_placement: Cell<bool>,
    pub search_field: Retained<NSSearchField>,
    pub auto_paste_checkbox: Retained<NSButton>,
    pub launch_at_login_checkbox: Retained<NSButton>,
    pub poll_popup: Retained<NSPopUpButton>,
    pub retention_popup: Retained<NSPopUpButton>,
    pub history_limit_popup: Retained<NSPopUpButton>,
    pub hotkey_popup: Retained<NSPopUpButton>,
    pub gear_button: Retained<NSButton>,
    pub hotkey_hint: Retained<NSTextField>,
    /// History list container (hidden when settings shown).
    pub history_container: Retained<NSView>,
    /// Settings panel (hidden by default).
    pub settings_panel: Retained<NSView>,
    /// Scroll document view; rows are laid out with fixed frames (no collapse).
    pub list_document: Retained<NSView>,
    pub scroll_view: Retained<NSScrollView>,
    pub empty_label: Retained<NSTextField>,
    pub status_label: Retained<NSTextField>,
    /// Compact action buttons (right side of history toolbar).
    pub select_button: Retained<NSButton>,
    pub delete_selected_button: Retained<NSButton>,
    pub clear_history_button: Retained<NSButton>,
    pub cancel_select_button: Retained<NSButton>,
    settings_visible: Cell<bool>,
    /// Multi-select mode for bulk delete.
    select_mode: Cell<bool>,
    selected_ids: RefCell<HashSet<u64>>,
    /// Keyboard cursor (Enter pastes it). Seeded from the ranker's prediction.
    highlight_id: Cell<Option<u64>>,
    /// Ranker's pick and its one-line reason (shown as a marker + tooltip).
    suggestion: RefCell<Option<(u64, String)>>,
    /// Row ids in on-screen order, top to bottom (for arrow-key navigation).
    display_order: RefCell<Vec<u64>>,
    /// Global mouse-down monitor so Transient popover closes when clicking outside.
    _dismiss_monitor: Option<Retained<AnyObject>>,
    _dismiss_block: Option<RcBlock<dyn Fn(NonNull<NSEvent>)>>,
}

impl StatusItemController {
    pub fn create(mtm: MainThreadMarker) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        if let Some(button) = status_item.button(mtm) {
            if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("clipboard"),
                Some(ns_string!("ZnonClip clipboard history")),
            ) {
                image.setTemplate(true);
                button.setImage(Some(&image));
            } else {
                button.setTitle(ns_string!("📋"));
            }
            button.setToolTip(Some(ns_string!("ZnonClip — Clipboard History")));
            // Open on left click AND right / two-finger click (default is left only).
            button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        }

        let built = build_popover(mtm);
        info!("status item and floater created");

        // A frame restored from autosave means the user has placed it before.
        let restored = built.panel.frame().origin.x != 0.0 || built.panel.frame().origin.y != 0.0;
        let float_on_top = Rc::new(Cell::new(true));

        // Outside click closes the floater unless "Float on top" is on.
        let panel_for_dismiss = built.panel.clone();
        let float_for_dismiss = float_on_top.clone();
        let status_item_for_dismiss = status_item.clone();
        let dismiss_block = RcBlock::new(move |event: NonNull<NSEvent>| {
            if !panel_for_dismiss.isVisible() || float_for_dismiss.get() {
                return;
            }
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            // SAFETY: event pointer is valid for this callback.
            let event = unsafe { event.as_ref() };
            let loc = event.locationInWindow();
            let screen_pt = if let Some(win) = event.window(mtm) {
                win.convertPointToScreen(loc)
            } else {
                loc
            };

            // Keep open if click is on the status item button.
            if let Some(btn) = status_item_for_dismiss.button(mtm) {
                if let Some(btn_win) = btn.window() {
                    let btn_frame =
                        btn_win.convertRectToScreen(btn.convertRect_toView(btn.bounds(), None));
                    if rect_contains_point(btn_frame, screen_pt) {
                        return;
                    }
                }
            }

            if rect_contains_point(panel_for_dismiss.frame(), screen_pt) {
                return;
            }
            info!("floater close via outside click");
            panel_for_dismiss.orderOut(None);
        });

        let dismiss_monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
            &dismiss_block,
        );

        Self {
            status_item,
            panel: built.panel,
            float_checkbox: built.float_checkbox,
            float_on_top,
            needs_initial_placement: Cell::new(!restored),
            search_field: built.search_field,
            auto_paste_checkbox: built.auto_paste_checkbox,
            launch_at_login_checkbox: built.launch_at_login_checkbox,
            poll_popup: built.poll_popup,
            retention_popup: built.retention_popup,
            history_limit_popup: built.history_limit_popup,
            hotkey_popup: built.hotkey_popup,
            gear_button: built.gear_button,
            hotkey_hint: built.hotkey_hint,
            history_container: built.history_container,
            settings_panel: built.settings_panel,
            list_document: built.list_document,
            scroll_view: built.scroll_view,
            empty_label: built.empty_label,
            status_label: built.status_label,
            select_button: built.select_button,
            delete_selected_button: built.delete_selected_button,
            clear_history_button: built.clear_history_button,
            cancel_select_button: built.cancel_select_button,
            settings_visible: Cell::new(false),
            select_mode: Cell::new(false),
            selected_ids: RefCell::new(HashSet::new()),
            highlight_id: Cell::new(None),
            suggestion: RefCell::new(None),
            display_order: RefCell::new(Vec::new()),
            _dismiss_monitor: dismiss_monitor,
            _dismiss_block: Some(dismiss_block),
        }
    }

    /// Wire controls to the app delegate.
    pub fn set_action_target(&self, target: &AnyObject) {
        unsafe {
            self.search_field.setTarget(Some(target));
            self.search_field.setAction(Some(sel!(searchFieldChanged:)));
            self.search_field.setContinuous(true);

            self.auto_paste_checkbox.setTarget(Some(target));
            self.auto_paste_checkbox
                .setAction(Some(sel!(toggleAutoPaste:)));

            self.launch_at_login_checkbox.setTarget(Some(target));
            self.launch_at_login_checkbox
                .setAction(Some(sel!(toggleLaunchAtLogin:)));

            self.poll_popup.setTarget(Some(target));
            self.poll_popup.setAction(Some(sel!(pollIntervalChanged:)));

            self.retention_popup.setTarget(Some(target));
            self.retention_popup
                .setAction(Some(sel!(retentionChanged:)));

            self.history_limit_popup.setTarget(Some(target));
            self.history_limit_popup
                .setAction(Some(sel!(historyLimitChanged:)));

            self.hotkey_popup.setTarget(Some(target));
            self.hotkey_popup.setAction(Some(sel!(hotkeyChanged:)));

            self.gear_button.setTarget(Some(target));
            self.gear_button.setAction(Some(sel!(toggleSettings:)));

            self.float_checkbox.setTarget(Some(target));
            self.float_checkbox.setAction(Some(sel!(toggleFloat:)));

            self.select_button.setTarget(Some(target));
            self.select_button.setAction(Some(sel!(toggleSelectMode:)));

            self.delete_selected_button.setTarget(Some(target));
            self.delete_selected_button
                .setAction(Some(sel!(deleteSelectedItems:)));

            self.clear_history_button.setTarget(Some(target));
            self.clear_history_button
                .setAction(Some(sel!(clearHistory:)));

            self.cancel_select_button.setTarget(Some(target));
            self.cancel_select_button
                .setAction(Some(sel!(cancelSelectMode:)));
        }
        self.refresh_toolbar_mode();
    }

    /// Set the ranker's suggestion and move the keyboard cursor onto it.
    pub fn set_suggestion(&self, suggestion: Option<(u64, String)>) {
        self.highlight_id.set(suggestion.as_ref().map(|s| s.0));
        *self.suggestion.borrow_mut() = suggestion;
    }

    /// Item under the keyboard cursor.
    pub fn highlighted_id(&self) -> Option<u64> {
        self.highlight_id.get()
    }

    /// Move the keyboard cursor by `delta` rows (clamped). Returns the new id.
    pub fn move_highlight(&self, delta: isize) -> Option<u64> {
        let order = self.display_order.borrow();
        if order.is_empty() {
            return None;
        }
        let cur = self
            .highlight_id
            .get()
            .and_then(|id| order.iter().position(|&x| x == id))
            .unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, order.len() as isize - 1) as usize;
        let id = order[next];
        self.highlight_id.set(Some(id));
        Some(id)
    }

    pub fn settings_visible(&self) -> bool {
        self.settings_visible.get()
    }

    pub fn is_select_mode(&self) -> bool {
        self.select_mode.get()
    }

    pub fn selected_ids(&self) -> Vec<u64> {
        self.selected_ids.borrow().iter().copied().collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected_ids.borrow().len()
    }

    /// Enter/exit multi-select mode (clears selection when leaving).
    pub fn set_select_mode(&self, on: bool) {
        self.select_mode.set(on);
        if !on {
            self.selected_ids.borrow_mut().clear();
        }
        self.refresh_toolbar_mode();
    }

    /// Toggle selection for one item. Returns new selected state.
    pub fn toggle_selection(&self, id: u64) -> bool {
        let mut set = self.selected_ids.borrow_mut();
        if set.contains(&id) {
            set.remove(&id);
            false
        } else {
            set.insert(id);
            true
        }
    }

    pub fn is_selected(&self, id: u64) -> bool {
        self.selected_ids.borrow().contains(&id)
    }

    pub fn clear_selection(&self) {
        self.selected_ids.borrow_mut().clear();
        self.refresh_toolbar_mode();
    }

    /// Show/hide Select vs Delete/Cancel controls based on mode.
    pub fn refresh_toolbar_mode(&self) {
        let select = self.select_mode.get();
        let n = self.selected_ids.borrow().len();

        self.auto_paste_checkbox.setHidden(select);
        self.select_button.setHidden(select);
        self.clear_history_button.setHidden(select);

        self.delete_selected_button.setHidden(!select);
        self.cancel_select_button.setHidden(!select);

        if select {
            let label = if n == 0 {
                "Delete".to_string()
            } else {
                format!("Delete ({n})")
            };
            self.delete_selected_button
                .setTitle(&NSString::from_str(&label));
            self.delete_selected_button.setEnabled(n > 0);
        }
    }

    /// Apply current settings values to UI controls.
    pub fn apply_settings_ui(&self, settings: &Settings) {
        self.set_auto_paste_checked(settings.auto_paste);
        self.set_launch_at_login_checked(settings.launch_at_login);
        self.set_float_on_top(settings.float_panel);
        self.poll_popup
            .selectItemAtIndex(settings.poll_interval_index() as isize);
        self.retention_popup
            .selectItemAtIndex(settings.retention_index() as isize);
        self.history_limit_popup
            .selectItemAtIndex(settings.history_limit_index() as isize);
        self.hotkey_popup
            .selectItemAtIndex(settings.hotkey.index() as isize);
        self.hotkey_hint
            .setStringValue(&NSString::from_str(settings.hotkey.display()));
    }

    /// Apply "Float on top": window level and whether outside clicks close it.
    pub fn set_float_on_top(&self, on: bool) {
        self.float_on_top.set(on);
        self.panel
            .setLevel(if on { NSFloatingWindowLevel } else { NSNormalWindowLevel });
        self.float_checkbox.setState(if on {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    pub fn is_float_checked(&self) -> bool {
        self.float_checkbox.state() == NSControlStateValueOn
    }

    pub fn float_on_top(&self) -> bool {
        self.float_on_top.get()
    }

    pub fn is_shown(&self) -> bool {
        self.panel.isVisible()
    }

    pub fn close(&self) {
        self.panel.orderOut(None);
    }

    /// Give keyboard focus back to the app underneath without hiding the
    /// floater: ordering a key non-activating panel out and straight back in
    /// returns key status to the active app's window.
    pub fn release_focus(&self) {
        if self.panel.isKeyWindow() {
            self.panel.orderOut(None);
            self.panel.orderFrontRegardless();
        }
    }

    pub fn set_auto_paste_checked(&self, checked: bool) {
        self.auto_paste_checkbox.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    pub fn is_auto_paste_checked(&self) -> bool {
        self.auto_paste_checkbox.state() == NSControlStateValueOn
    }

    pub fn set_launch_at_login_checked(&self, checked: bool) {
        self.launch_at_login_checkbox.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    pub fn is_launch_at_login_checked(&self) -> bool {
        self.launch_at_login_checkbox.state() == NSControlStateValueOn
    }

    pub fn selected_poll_ms(&self) -> u64 {
        let i = self.poll_popup.indexOfSelectedItem().max(0) as usize;
        POLL_INTERVAL_OPTIONS
            .get(i)
            .copied()
            .unwrap_or(500)
    }

    pub fn selected_retention_days(&self) -> i64 {
        let i = self.retention_popup.indexOfSelectedItem().max(0) as usize;
        RETENTION_DAY_OPTIONS.get(i).copied().unwrap_or(30)
    }

    pub fn selected_history_limit(&self) -> usize {
        let i = self.history_limit_popup.indexOfSelectedItem().max(0) as usize;
        HISTORY_LIMIT_OPTIONS.get(i).copied().unwrap_or(DEFAULT_HISTORY_ITEMS)
    }

    pub fn selected_hotkey(&self) -> HotkeyPreset {
        let i = self.hotkey_popup.indexOfSelectedItem().max(0) as usize;
        HotkeyPreset::from_index(i)
    }

    /// Toggle settings panel vs history list.
    pub fn toggle_settings_panel(&self) {
        let show = !self.settings_visible.get();
        self.settings_visible.set(show);
        self.settings_panel.setHidden(!show);
        self.history_container.setHidden(show);
        if show {
            if let Some(img) = system_symbol("xmark", "Close settings", 12.0) {
                self.gear_button.setImage(Some(&img));
                self.gear_button.setTitle(ns_string!(""));
            } else {
                self.gear_button.setTitle(ns_string!("Close"));
            }
            self.gear_button
                .setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            self.gear_button.setToolTip(Some(ns_string!("Back to history")));
        } else {
            if let Some(img) = system_symbol("gearshape", "Settings", 13.0) {
                self.gear_button.setImage(Some(&img));
                self.gear_button.setTitle(ns_string!(""));
            } else {
                self.gear_button.setTitle(ns_string!("Settings"));
            }
            self.gear_button
                .setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            self.gear_button.setToolTip(Some(ns_string!("Settings")));
        }
    }

    pub fn show_history(&self) {
        if self.settings_visible.get() {
            self.toggle_settings_panel();
        }
    }

    /// Show or hide the floater. Showing makes it key (search + arrow keys)
    /// without activating this app.
    pub fn toggle_popover(&self, mtm: MainThreadMarker) {
        if self.panel.isVisible() {
            self.panel.orderOut(None);
            return;
        }

        if self.needs_initial_placement.replace(false) {
            // First open ever: hang it just below the menu-bar icon.
            if let Some(button) = self.status_item.button(mtm) {
                if let Some(win) = button.window() {
                    let r = win.convertRectToScreen(button.convertRect_toView(button.bounds(), None));
                    let w = self.panel.frame().size.width;
                    self.panel
                        .setFrameTopLeftPoint(NSPoint::new((r.origin.x + r.size.width / 2.0 - w / 2.0).max(8.0), r.origin.y - 6.0));
                }
            }
        }

        self.panel.orderFrontRegardless();
        self.panel.makeKeyWindow();

        if !self.settings_visible.get() {
            let _ = self.panel.makeFirstResponder(Some(&self.search_field));
        }
    }

    pub fn search_query(&self) -> String {
        self.search_field.stringValue().to_string()
    }

    pub fn refresh_history(
        &self,
        mtm: MainThreadMarker,
        history: &History,
        action_target: &AnyObject,
    ) {
        let query = self.search_query();
        let items = history.filter(&query);
        self.render_items(mtm, &items, action_target, query.trim().is_empty());
    }

    pub fn render_items(
        &self,
        mtm: MainThreadMarker,
        items: &[ClipboardItem],
        action_target: &AnyObject,
        query_empty: bool,
    ) {
        // Clear previous rows (fixed-frame document view).
        let existing = self.list_document.subviews();
        for view in existing.iter() {
            view.removeFromSuperview();
        }

        // Drop selection ids that are no longer visible/present.
        {
            let visible: HashSet<u64> = items.iter().map(|i| i.id).collect();
            self.selected_ids
                .borrow_mut()
                .retain(|id| visible.contains(id));
        }
        self.refresh_toolbar_mode();

        let list_w = self.scroll_view.contentView().bounds().size.width.max(MIN_W - 20.0);
        let content_w = list_w - PAD * 2.0;
        let clip_h = self.scroll_view.contentView().bounds().size.height;

        if items.is_empty() {
            self.display_order.borrow_mut().clear();
            self.highlight_id.set(None);
            if query_empty {
                self.empty_label.setStringValue(ns_string!(
                    "No history yet\nCopy something to get started"
                ));
            } else {
                self.empty_label
                    .setStringValue(ns_string!("No matches"));
            }
            let doc_h = clip_h.max(120.0);
            self.list_document
                .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(list_w, doc_h)));
            self.empty_label.setFrame(NSRect::new(
                NSPoint::new(PAD, doc_h / 2.0 - 24.0),
                NSSize::new(content_w, 48.0),
            ));
            self.list_document.addSubview(&self.empty_label);
            return;
        }

        let pinned: Vec<&ClipboardItem> = items.iter().filter(|i| i.is_pinned).collect();
        let recent: Vec<&ClipboardItem> = items.iter().filter(|i| !i.is_pinned).collect();

        // On-screen order drives arrow keys; a stale cursor falls back to the top row.
        let order: Vec<u64> = pinned.iter().chain(recent.iter()).map(|i| i.id).collect();
        if !self.highlight_id.get().is_some_and(|id| order.contains(&id)) {
            self.highlight_id.set(order.first().copied());
        }
        *self.display_order.borrow_mut() = order;
        let cursor = if self.select_mode.get() { None } else { self.highlight_id.get() };
        let suggestion = self.suggestion.borrow().clone();
        let mut cursor_frame: Option<NSRect> = None;

        // Compute total document height (top → bottom sections).
        let mut content_h = 4.0_f64;
        if !pinned.is_empty() {
            content_h += SECTION_H + 2.0;
            content_h += pinned.len() as f64 * (ROW_H + ROW_GAP);
            if !recent.is_empty() {
                content_h += SEP_H + 6.0;
            }
        }
        if !recent.is_empty() {
            // Show "Recent" header only when there is also a pinned section.
            if !pinned.is_empty() {
                content_h += SECTION_H + 2.0;
            }
            content_h += recent.len() as f64 * (ROW_H + ROW_GAP);
        }
        content_h += 4.0;
        let doc_h = content_h.max(clip_h);
        self.list_document
            .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(list_w, doc_h)));

        // Lay out from the top of the document (y grows upward in AppKit).
        let mut y = doc_h - 4.0;
        let select = self.select_mode.get();

        if !pinned.is_empty() {
            y -= SECTION_H;
            let header = section_header(mtm, "Pinned", PAD, y, content_w);
            self.list_document.addSubview(&header);
            y -= 4.0;

            for item in &pinned {
                y -= ROW_H;
                let selected = self.is_selected(item.id) || cursor == Some(item.id);
                let hint = suggestion.as_ref().filter(|s| s.0 == item.id).map(|s| s.1.as_str());
                let row = make_history_row(mtm, item, action_target, select, selected, hint, content_w);
                let frame = NSRect::new(NSPoint::new(PAD, y), NSSize::new(content_w, ROW_H));
                row.setFrame(frame);
                if cursor == Some(item.id) {
                    cursor_frame = Some(frame);
                }
                self.list_document.addSubview(&row);
                y -= ROW_GAP;
            }

            if !recent.is_empty() {
                y -= 6.0;
                y -= SEP_H;
                let sep = section_separator(mtm, PAD, y, content_w);
                self.list_document.addSubview(&sep);
                y -= 6.0;

                y -= SECTION_H;
                let header = section_header(mtm, "Recent", PAD, y, content_w);
                self.list_document.addSubview(&header);
                y -= 4.0;
            }
        }

        for item in &recent {
            y -= ROW_H;
            let selected = self.is_selected(item.id) || cursor == Some(item.id);
            let hint = suggestion.as_ref().filter(|s| s.0 == item.id).map(|s| s.1.as_str());
            let row = make_history_row(mtm, item, action_target, select, selected, hint, content_w);
            let frame = NSRect::new(NSPoint::new(PAD, y), NSSize::new(content_w, ROW_H));
            row.setFrame(frame);
            if cursor == Some(item.id) {
                cursor_frame = Some(frame);
            }
            self.list_document.addSubview(&row);
            y -= ROW_GAP;
        }

        // Keep newest content visible at the top of the scroll view.
        let clip = self.scroll_view.contentView();
        let max_y = (doc_h - clip.bounds().size.height).max(0.0);
        clip.scrollToPoint(NSPoint::new(0.0, max_y));
        self.scroll_view.reflectScrolledClipView(&clip);
        // Then make sure the keyboard cursor is on screen.
        if let Some(frame) = cursor_frame {
            let _ = self.list_document.scrollRectToVisible(frame);
        }
    }

    pub fn set_status_notice(&self, message: Option<&str>) {
        match message {
            Some(msg) => {
                self.status_label
                    .setStringValue(&NSString::from_str(msg));
                self.status_label.setHidden(false);
            }
            None => {
                self.status_label.setStringValue(ns_string!(""));
                self.status_label.setHidden(true);
            }
        }
    }
}

struct PopoverParts {
    panel: Retained<NSPanel>,
    float_checkbox: Retained<NSButton>,
    search_field: Retained<NSSearchField>,
    auto_paste_checkbox: Retained<NSButton>,
    launch_at_login_checkbox: Retained<NSButton>,
    poll_popup: Retained<NSPopUpButton>,
    retention_popup: Retained<NSPopUpButton>,
    history_limit_popup: Retained<NSPopUpButton>,
    hotkey_popup: Retained<NSPopUpButton>,
    gear_button: Retained<NSButton>,
    hotkey_hint: Retained<NSTextField>,
    history_container: Retained<NSView>,
    settings_panel: Retained<NSView>,
    list_document: Retained<NSView>,
    scroll_view: Retained<NSScrollView>,
    empty_label: Retained<NSTextField>,
    status_label: Retained<NSTextField>,
    select_button: Retained<NSButton>,
    delete_selected_button: Retained<NSButton>,
    clear_history_button: Retained<NSButton>,
    cancel_select_button: Retained<NSButton>,
}

fn build_popover(mtm: MainThreadMarker) -> PopoverParts {
    let root = NSView::new(mtm);
    root.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT),
    ));
    root.setWantsLayer(true);
    root.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);

    // Resize rules: header pinned top, right-side controls pinned right,
    // the list and search stretch.
    let top = NSAutoresizingMaskOptions::ViewMinYMargin;
    let top_right = NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewMinXMargin;
    let top_wide = NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewWidthSizable;
    let fill = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;

    // Leave room for the (transparent) title bar strip used for dragging.
    let header_y = POPOVER_HEIGHT - HEADER_H - 14.0;

    // Wordmark — medium weight, primary label (not heavy bold).
    let header = NSTextField::labelWithString(ns_string!("ZnonClip"), mtm);
    header.setFrame(NSRect::new(
        NSPoint::new(PAD, header_y),
        NSSize::new(140.0, HEADER_H - 6.0),
    ));
    header.setAutoresizingMask(top);
    header.setFont(Some(unsafe { NSFont::systemFontOfSize_weight(13.0, NSFontWeightSemibold) }.as_ref()));
    header.setTextColor(Some(&NSColor::labelColor()));
    root.addSubview(&header);

    // Hotkey chip (quiet monospaced digits — signature of macOS utilities).
    // "Float on top": keep the floater above other windows and open after a paste.
    let float_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(ns_string!("Float on top"), None, None, mtm)
    };
    float_checkbox.setFrame(NSRect::new(
        NSPoint::new(POPOVER_WIDTH - PAD - 92.0 - 100.0, header_y + 1.0),
        NSSize::new(98.0, 18.0),
    ));
    float_checkbox.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    float_checkbox.setToolTip(Some(ns_string!(
        "On: stays above other windows and open after a paste. Off: closes on paste or an outside click."
    )));
    float_checkbox.setAutoresizingMask(top_right);
    root.addSubview(&float_checkbox);

    let hotkey_hint = NSTextField::labelWithString(ns_string!("⌃⌘V"), mtm);
    hotkey_hint.setFrame(NSRect::new(
        NSPoint::new(POPOVER_WIDTH - PAD - 92.0, header_y + 2.0),
        NSSize::new(52.0, 16.0),
    ));
    hotkey_hint.setFont(Some(unsafe { NSFont::monospacedDigitSystemFontOfSize_weight(11.0, NSFontWeightRegular) }.as_ref()));
    hotkey_hint.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    hotkey_hint.setAlignment(NSTextAlignment::Right);
    hotkey_hint.setToolTip(Some(ns_string!("Global hotkey")));
    hotkey_hint.setAutoresizingMask(top_right);
    root.addSubview(&hotkey_hint);

    // Gear — borderless SF Symbol, secondary tint.
    let gear_button = if let Some(gear_img) = system_symbol("gearshape", "Settings", 13.0) {
        unsafe { NSButton::buttonWithImage_target_action(&gear_img, None, None, mtm) }
    } else {
        unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Settings"), None, None, mtm) }
    };
    gear_button.setBordered(false);
    gear_button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    gear_button.setFrame(NSRect::new(
        NSPoint::new(POPOVER_WIDTH - PAD - 26.0, header_y - 1.0),
        NSSize::new(26.0, 22.0),
    ));
    gear_button.setToolTip(Some(ns_string!("Settings")));
    gear_button.setAutoresizingMask(top_right);
    root.addSubview(&gear_button);

    // Hairline under header.
    let header_sep = hairline(mtm, PAD, header_y - 6.0, POPOVER_WIDTH - PAD * 2.0);
    header_sep.setAutoresizingMask(top_wide);
    root.addSubview(&header_sep);

    // --- History container ---
    let history_top = header_y - 10.0;
    let history_container = NSView::new(mtm);
    history_container.setFrame(NSRect::new(
        NSPoint::new(0.0, STATUS_H),
        NSSize::new(POPOVER_WIDTH, history_top - STATUS_H),
    ));
    history_container.setAutoresizingMask(fill);

    let search_field = NSSearchField::new(mtm);
    search_field.setFrame(NSRect::new(
        NSPoint::new(PAD, history_container.frame().size.height - SEARCH_H - 6.0),
        NSSize::new(POPOVER_WIDTH - PAD * 2.0, SEARCH_H),
    ));
    search_field.setPlaceholderString(Some(ns_string!("Search")));
    search_field.setAutoresizingMask(top_wide);
    history_container.addSubview(&search_field);

    // Toolbar: quiet secondary actions — [Auto-paste] …… [Select] [Clear]
    let toolbar_y = history_container.frame().size.height - SEARCH_H - TOOLBAR_H - 12.0;

    let auto_paste_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(ns_string!("Auto-paste"), None, None, mtm)
    };
    auto_paste_checkbox.setFrame(NSRect::new(
        NSPoint::new(PAD, toolbar_y),
        NSSize::new(108.0, TOOLBAR_H),
    ));
    auto_paste_checkbox.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    auto_paste_checkbox.setAutoresizingMask(top);
    auto_paste_checkbox.setToolTip(Some(ns_string!(
        "After copying an item, simulate ⌘V into the frontmost app."
    )));
    history_container.addSubview(&auto_paste_checkbox);

    let btn_w = 58.0;
    let btn_h = TOOLBAR_H;
    let right = POPOVER_WIDTH - PAD;

    let clear_history_button =
        make_toolbar_text_button(mtm, "Clear", right - btn_w, toolbar_y, btn_w, btn_h, false);
    clear_history_button.setToolTip(Some(ns_string!(
        "Clear unpinned history (pinned items are kept)"
    )));
    clear_history_button.setAutoresizingMask(top_right);
    history_container.addSubview(&clear_history_button);

    let select_button = make_toolbar_text_button(
        mtm,
        "Select",
        right - btn_w * 2.0 - 4.0,
        toolbar_y,
        btn_w,
        btn_h,
        false,
    );
    select_button.setToolTip(Some(ns_string!("Select multiple items to delete")));
    select_button.setAutoresizingMask(top_right);
    history_container.addSubview(&select_button);

    let delete_selected_button = make_toolbar_text_button(
        mtm,
        "Delete",
        right - btn_w * 2.0 - 4.0,
        toolbar_y,
        btn_w + 24.0,
        btn_h,
        true, // destructive
    );
    delete_selected_button.setToolTip(Some(ns_string!("Delete selected items")));
    delete_selected_button.setHidden(true);
    delete_selected_button.setEnabled(false);
    delete_selected_button.setAutoresizingMask(top_right);
    history_container.addSubview(&delete_selected_button);

    let cancel_select_button =
        make_toolbar_text_button(mtm, "Cancel", right - btn_w, toolbar_y, btn_w, btn_h, false);
    cancel_select_button.setToolTip(Some(ns_string!("Exit selection mode")));
    cancel_select_button.setHidden(true);
    cancel_select_button.setAutoresizingMask(top_right);
    history_container.addSubview(&cancel_select_button);

    let list_top = SEARCH_H + TOOLBAR_H + 18.0;
    let list_height = history_container.frame().size.height - list_top;

    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(POPOVER_WIDTH, list_height),
    ));
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
    scroll.setAutohidesScrollers(true);
    scroll.setAutoresizingMask(fill);

    let list_document = NSView::new(mtm);
    list_document.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(POPOVER_WIDTH, list_height),
    ));

    let empty_label = NSTextField::labelWithString(
        ns_string!("No history yet\nCopy something to get started"),
        mtm,
    );
    empty_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    empty_label.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    empty_label.setMaximumNumberOfLines(3);
    empty_label.setPreferredMaxLayoutWidth(POPOVER_WIDTH - PAD * 2.0);
    empty_label.setAlignment(NSTextAlignment::Center);

    scroll.setDocumentView(Some(&list_document));
    history_container.addSubview(&scroll);
    root.addSubview(&history_container);

    // --- Settings panel ---
    let settings_panel = build_settings_panel(mtm);
    settings_panel.setAutoresizingMask(fill);
    for v in settings_panel.subviews().iter() {
        v.setAutoresizingMask(top);
    }
    settings_panel.setHidden(true);
    root.addSubview(&settings_panel);

    // Status line — tertiary, single line, non-intrusive.
    let status_label = NSTextField::labelWithString(ns_string!(""), mtm);
    status_label.setFrame(NSRect::new(
        NSPoint::new(PAD, 3.0),
        NSSize::new(POPOVER_WIDTH - PAD * 2.0, STATUS_H - 6.0),
    ));
    status_label.setFont(Some(&NSFont::systemFontOfSize(10.0)));
    status_label.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    status_label.setMaximumNumberOfLines(1);
    status_label.setPreferredMaxLayoutWidth(POPOVER_WIDTH - PAD * 2.0);
    status_label.setHidden(true);
    status_label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    root.addSubview(&status_label);

    // Translucent background (menu-bar material) behind everything.
    let effect = NSVisualEffectView::new(mtm);
    effect.setFrame(root.frame());
    effect.setMaterial(NSVisualEffectMaterial::Popover);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setAutoresizingMask(fill);
    effect.addSubview(&root);

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView
        | NSWindowStyleMask::NonactivatingPanel
        | NSWindowStyleMask::UtilityWindow;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT)),
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: we keep a strong reference for the life of the app.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    panel.setTitlebarAppearsTransparent(true);
    for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(btn) = panel.standardWindowButton(b) {
            btn.setHidden(true);
        }
    }
    panel.setMovableByWindowBackground(true);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHidesOnDeactivate(false);
    panel.setFloatingPanel(true);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setMinSize(NSSize::new(MIN_W, MIN_H));
    panel.setContentView(Some(&effect));
    // Restores the last size/position if one was saved.
    let _ = panel.setFrameAutosaveName(&NSString::from_str(FRAME_AUTOSAVE));

    // Extract settings controls (retained on panel as subviews; also keep named refs)
    let (
        launch_at_login_checkbox,
        poll_popup,
        retention_popup,
        history_limit_popup,
        hotkey_popup,
    ) = extract_settings_controls(&settings_panel);

    PopoverParts {
        panel,
        float_checkbox,
        search_field,
        auto_paste_checkbox,
        launch_at_login_checkbox,
        poll_popup,
        retention_popup,
        history_limit_popup,
        hotkey_popup,
        gear_button,
        hotkey_hint,
        history_container,
        settings_panel,
        list_document,
        scroll_view: scroll,
        empty_label,
        status_label,
        select_button,
        delete_selected_button,
        clear_history_button,
        cancel_select_button,
    }
}

fn section_header(
    mtm: MainThreadMarker,
    title: &str,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSTextField> {
    // Uppercase secondary labels match macOS utility sections
    // (e.g. "Recent Network Activity").
    let label = NSTextField::labelWithString(&NSString::from_str(&title.to_ascii_uppercase()), mtm);
    label.setFrame(NSRect::new(NSPoint::new(x + 2.0, y), NSSize::new(w - 2.0, SECTION_H)));
    label.setFont(Some(unsafe { NSFont::systemFontOfSize_weight(11.0, NSFontWeightMedium) }.as_ref()));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setAlignment(NSTextAlignment::Left);
    label
}

fn section_separator(mtm: MainThreadMarker, x: f64, y: f64, w: f64) -> Retained<NSBox> {
    hairline(mtm, x, y, w)
}

fn hairline(mtm: MainThreadMarker, x: f64, y: f64, w: f64) -> Retained<NSBox> {
    let box_ = NSBox::new(mtm);
    box_.setBoxType(NSBoxType::Custom);
    box_.setTitlePosition(NSTitlePosition::NoTitle);
    box_.setBorderWidth(0.0);
    box_.setCornerRadius(0.0);
    box_.setFillColor(&NSColor::separatorColor());
    box_.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, SEP_H)));
    box_
}

fn make_toolbar_text_button(
    mtm: MainThreadMarker,
    title: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    destructive: bool,
) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    // Borderless so contentTintColor applies (system secondary / red).
    button.setBordered(false);
    button.setFont(Some(
        unsafe { NSFont::systemFontOfSize_weight(11.0, NSFontWeightMedium) }.as_ref(),
    ));
    button.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
    button.setAlignment(NSTextAlignment::Center);
    if destructive {
        button.setContentTintColor(Some(&NSColor::systemRedColor()));
    } else {
        button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    }
    button
}

/// Build settings panel and return it; controls are tagged for lookup.
///
/// Layout uses a top-edge cursor (`top`): each control is placed with its
/// *top* at `top`, then `top` moves down by height + gap. AppKit frames still
/// use bottom-left origin (`bottom = top - height`).
fn build_settings_panel(mtm: MainThreadMarker) -> Retained<NSView> {
    let panel = NSView::new(mtm);
    panel.setFrame(NSRect::new(
        NSPoint::new(0.0, STATUS_H),
        NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT - HEADER_H - STATUS_H - 10.0),
    ));

    let mut top = panel.frame().size.height - 14.0;
    let label_w = 112.0;
    let control_x = PAD + label_w + 10.0;
    let control_w = POPOVER_WIDTH - control_x - PAD;
    let popup_h = 26.0;
    let row_gap = 10.0;
    let group_gap = 20.0;
    let header_gap = 6.0;

    // ── History ──────────────────────────────────────────────────────────
    top = place_settings_section(mtm, &panel, "History", top, header_gap);

    let poll_y = top - popup_h;
    add_field_label(mtm, &panel, "Poll interval", PAD, poll_y, popup_h);
    let poll_popup = make_popup(mtm, control_x, poll_y, control_w);
    for ms in POLL_INTERVAL_OPTIONS {
        poll_popup.addItemWithTitle(&NSString::from_str(&poll_label(ms)));
    }
    poll_popup.setTag(1);
    panel.addSubview(&poll_popup);
    top = poll_y - row_gap;

    let ret_y = top - popup_h;
    add_field_label(mtm, &panel, "Keep history", PAD, ret_y, popup_h);
    let retention_popup = make_popup(mtm, control_x, ret_y, control_w);
    for d in RETENTION_DAY_OPTIONS {
        retention_popup.addItemWithTitle(&NSString::from_str(&retention_label(d)));
    }
    retention_popup.setTag(2);
    panel.addSubview(&retention_popup);
    top = ret_y - row_gap;

    let max_y = top - popup_h;
    add_field_label(mtm, &panel, "Max items", PAD, max_y, popup_h);
    let history_limit_popup = make_popup(mtm, control_x, max_y, control_w);
    for n in HISTORY_LIMIT_OPTIONS {
        history_limit_popup.addItemWithTitle(&NSString::from_str(&history_limit_label(n)));
    }
    history_limit_popup.setTag(3);
    panel.addSubview(&history_limit_popup);
    top = max_y - group_gap;

    // ── Shortcuts ────────────────────────────────────────────────────────
    top = place_settings_section(mtm, &panel, "Shortcuts", top, header_gap);

    let hot_y = top - popup_h;
    add_field_label(mtm, &panel, "Hotkey", PAD, hot_y, popup_h);
    let hotkey_popup = make_popup(mtm, control_x, hot_y, control_w);
    for p in HotkeyPreset::ALL {
        hotkey_popup.addItemWithTitle(&NSString::from_str(p.display()));
    }
    hotkey_popup.setTag(4);
    panel.addSubview(&hotkey_popup);
    top = hot_y - group_gap;

    // ── General ──────────────────────────────────────────────────────────
    top = place_settings_section(mtm, &panel, "General", top, header_gap);

    let cb_h = 22.0;
    let cb_y = top - cb_h;
    let launch_cb = unsafe {
        NSButton::checkboxWithTitle_target_action(ns_string!("Launch at login"), None, None, mtm)
    };
    launch_cb.setFrame(NSRect::new(
        NSPoint::new(PAD, cb_y),
        NSSize::new(POPOVER_WIDTH - PAD * 2.0, cb_h),
    ));
    launch_cb.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    launch_cb.setToolTip(Some(ns_string!(
        "Start ZnonClip when you log in. Uses SMAppService for .app installs, or a LaunchAgent for cargo/dev builds."
    )));
    launch_cb.setTag(5);
    panel.addSubview(&launch_cb);
    top = cb_y - 8.0;

    let note_h = 28.0;
    let note_y = top - note_h;
    let note = NSTextField::labelWithString(
        ns_string!("Packaged apps use Login Items. Dev builds use a LaunchAgent."),
        mtm,
    );
    note.setFrame(NSRect::new(
        NSPoint::new(PAD + 2.0, note_y),
        NSSize::new(POPOVER_WIDTH - PAD * 2.0 - 2.0, note_h),
    ));
    note.setFont(Some(&NSFont::systemFontOfSize(10.0)));
    note.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    note.setMaximumNumberOfLines(2);
    note.setPreferredMaxLayoutWidth(POPOVER_WIDTH - PAD * 2.0 - 2.0);
    panel.addSubview(&note);

    let _ = (
        &poll_popup,
        &retention_popup,
        &history_limit_popup,
        &hotkey_popup,
        &launch_cb,
    );

    panel
}

/// Place uppercase section header under `top`; return new top below it + gap.
fn place_settings_section(
    mtm: MainThreadMarker,
    panel: &NSView,
    title: &str,
    top: f64,
    gap_after: f64,
) -> f64 {
    let bottom = top - SECTION_H;
    let header = section_header(mtm, title, PAD, bottom, POPOVER_WIDTH - PAD * 2.0);
    panel.addSubview(&header);
    bottom - gap_after
}

fn extract_settings_controls(
    panel: &NSView,
) -> (
    Retained<NSButton>,
    Retained<NSPopUpButton>,
    Retained<NSPopUpButton>,
    Retained<NSPopUpButton>,
    Retained<NSPopUpButton>,
) {
    let mut launch = None;
    let mut poll = None;
    let mut retention = None;
    let mut history = None;
    let mut hotkey = None;

    for view in panel.subviews().iter() {
        if let Some(btn) = view.downcast_ref::<NSButton>() {
            if btn.tag() == 5 {
                launch = Some(btn.retain());
            }
        }
        if let Some(pop) = view.downcast_ref::<NSPopUpButton>() {
            match pop.tag() {
                1 => poll = Some(pop.retain()),
                2 => retention = Some(pop.retain()),
                3 => history = Some(pop.retain()),
                4 => hotkey = Some(pop.retain()),
                _ => {}
            }
        }
    }

    (
        launch.expect("launch checkbox"),
        poll.expect("poll popup"),
        retention.expect("retention popup"),
        history.expect("history popup"),
        hotkey.expect("hotkey popup"),
    )
}

fn add_field_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    control_bottom: f64,
    control_h: f64,
) {
    // Vertically center a 16pt label against the popup/control height.
    let label_h = 16.0;
    let label_y = control_bottom + (control_h - label_h) / 2.0;
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(NSRect::new(
        NSPoint::new(x, label_y),
        NSSize::new(112.0, label_h),
    ));
    label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    label.setTextColor(Some(&NSColor::labelColor()));
    label.setEditable(false);
    label.setSelectable(false);
    parent.addSubview(&label);
}

fn make_popup(mtm: MainThreadMarker, x: f64, y: f64, w: f64) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, 26.0)),
        false,
    );
    popup
}

/// Composite history row: rounded selection fill + icon + preview + relative time.
/// Matches macOS utility lists (icon | primary | trailing meta).
fn make_history_row(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    action_target: &AnyObject,
    select_mode: bool,
    selected: bool,
    suggestion: Option<&str>,
    width: f64,
) -> Retained<NSView> {
    let row = NSView::new(mtm);
    row.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(width, ROW_H),
    ));

    if selected {
        let bg = NSBox::new(mtm);
        bg.setBoxType(NSBoxType::Custom);
        bg.setTitlePosition(NSTitlePosition::NoTitle);
        bg.setBorderWidth(0.0);
        bg.setCornerRadius(ROW_RADIUS);
        bg.setFillColor(&NSColor::controlAccentColor().colorWithAlphaComponent(0.18));
        bg.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width, ROW_H),
        ));
        row.addSubview(&bg);
    }

    let action = if select_mode {
        sel!(toggleItemSelection:)
    } else {
        sel!(copyHistoryItem:)
    };

    let preview = truncate_chars(&item.preview, 40);
    // Primary hit target spans icon + preview; trailing time stays out of the way.
    let hit_w = (width - TIME_COL - 6.0).max(80.0);
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(&preview),
            Some(action_target),
            Some(action),
            mtm,
        )
    };
    button.setBordered(!selected);
    if !selected {
        button.setBezelStyle(NSBezelStyle::AccessoryBar);
        button.setShowsBorderOnlyWhileMouseInside(true);
    } else {
        button.setBordered(false);
    }
    button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    button.setAlignment(NSTextAlignment::Left);
    button.setTag(item.id as isize);
    button.setImagePosition(NSCellImagePosition::ImageLeft);
    button.setImageScaling(NSImageScaling::ScaleProportionallyDown);
    button.setUsesSingleLineMode(true);
    button.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    button.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(hit_w, ROW_H),
    ));

    let symbol_name = if select_mode {
        if selected {
            "checkmark.circle.fill"
        } else {
            "circle"
        }
    } else if suggestion.is_some() {
        "sparkles"
    } else if item.is_pinned {
        "pin.fill"
    } else {
        item.content_type.sf_symbol()
    };
    if let Some(image) = system_symbol(symbol_name, item.content_type.label(), 12.0) {
        button.setImage(Some(&image));
    }
    // Do not set contentTintColor here — it would recolor the title text as well.
    // Template SF Symbols already render in the correct secondary system style.

    let tip = item
        .content_text
        .as_deref()
        .unwrap_or(item.preview.as_str());
    let tip = if tip.chars().count() > 400 {
        format!("{}…", tip.chars().take(400).collect::<String>())
    } else {
        tip.to_string()
    };
    let tip = if select_mode {
        if selected {
            format!("Selected · {tip}")
        } else {
            format!("Click to select · {tip}")
        }
    } else {
        let mut prefix = String::new();
        if let Some(reason) = suggestion {
            prefix.push_str(&format!("Suggested ({reason}) · Enter to paste · "));
        }
        if item.is_pinned {
            prefix.push_str("Pinned · ");
        }
        format!("{prefix}{tip}")
    };
    button.setToolTip(Some(&NSString::from_str(&tip)));

    if !select_mode {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let copy_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Copy"),
                Some(sel!(copyHistoryItem:)),
                ns_string!(""),
            )
        };
        copy_item.setTag(item.id as isize);
        unsafe { copy_item.setTarget(Some(action_target)) };

        let pin_title = if item.is_pinned {
            ns_string!("Unpin")
        } else {
            ns_string!("Pin")
        };
        let pin_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                pin_title,
                Some(sel!(pinHistoryItem:)),
                ns_string!(""),
            )
        };
        pin_item.setTag(item.id as isize);
        unsafe { pin_item.setTarget(Some(action_target)) };

        let del_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Delete"),
                Some(sel!(deleteHistoryItem:)),
                ns_string!(""),
            )
        };
        del_item.setTag(item.id as isize);
        unsafe { del_item.setTarget(Some(action_target)) };

        unsafe { button.setMenu(Some(&menu)) };
    }

    row.addSubview(&button);

    let rel = format_relative_time(&item.created_at);
    if !rel.is_empty() {
        let time_label = NSTextField::labelWithString(&NSString::from_str(&rel), mtm);
        time_label.setFrame(NSRect::new(
            NSPoint::new(width - TIME_COL - 4.0, (ROW_H - 14.0) / 2.0),
            NSSize::new(TIME_COL, 14.0),
        ));
        time_label.setFont(Some(unsafe { NSFont::monospacedDigitSystemFontOfSize_weight(10.0, NSFontWeightRegular) }.as_ref()));
        time_label.setTextColor(Some(&NSColor::tertiaryLabelColor()));
        time_label.setAlignment(NSTextAlignment::Right);
        time_label.setEditable(false);
        time_label.setSelectable(false);
        row.addSubview(&time_label);
    }

    row
}

/// System SF Symbol sized for list UI (template rendering).
fn system_symbol(name: &str, a11y: &str, point_size: f64) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(a11y)),
    )?;
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight_scale(
        point_size,
        unsafe { NSFontWeightRegular },
        NSImageSymbolScale::Medium,
    );
    let configured = image.imageWithSymbolConfiguration(&config)?;
    configured.setTemplate(true);
    Some(configured)
}

/// Compact relative time for trailing meta column: "now", "1m", "2h", "3d".
fn format_relative_time(created_at: &str) -> String {
    let Some(secs) = parse_iso8601_to_unix_secs(created_at) else {
        return String::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(secs);
    let mut delta = now - secs;
    if delta < 0 {
        delta = 0;
    }

    if delta < 45 {
        return "now".into();
    }
    if delta < 90 {
        return "1m".into();
    }
    if delta < 3600 {
        return format!("{}m", delta / 60);
    }
    if delta < 3600 * 36 {
        return format!("{}h", delta / 3600);
    }
    if delta < 86400 * 14 {
        return format!("{}d", delta / 86400);
    }
    let w = delta / (86400 * 7);
    if w < 9 {
        return format!("{w}w");
    }
    let mo = delta / (86400 * 30);
    if mo < 18 {
        return format!("{mo}mo");
    }
    format!("{}y", delta / (86400 * 365))
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.mmm]Z` (ZnonClip storage format) to unix seconds.
pub(crate) fn parse_iso8601_to_unix_secs(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i32 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let h: u32 = t.next()?.parse().ok()?;
    let mi: u32 = t.next()?.parse().ok()?;
    let sec: u32 = t.next()?.parse().ok()?;
    Some(civil_to_unix_secs(y, mo, day, h, mi, sec))
}

fn civil_to_unix_secs(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    // Howard Hinnant civil_from_days inverse (days_from_civil).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era as i64) * 146_097 + doe as i64 - 719_468;
    days * 86_400 + (h as i64) * 3600 + (mi as i64) * 60 + s as i64
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn rect_contains_point(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.width
        && point.y < rect.origin.y + rect.size.height
}

/// Extract history item id from an NSControl or NSMenuItem sender.
pub fn sender_item_id(sender: Option<&AnyObject>) -> Option<u64> {
    let sender = sender?;
    if let Some(control) = sender.downcast_ref::<NSControl>() {
        let tag = control.tag();
        if tag > 0 {
            return Some(tag as u64);
        }
    }
    if let Some(item) = sender.downcast_ref::<NSMenuItem>() {
        let tag = item.tag();
        if tag > 0 {
            return Some(tag as u64);
        }
    }
    None
}
