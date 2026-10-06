//! One history row in the floater.
//!
//! Layout, left to right: a thumbnail (images) or type tile, a title over a
//! monospaced meta line, and an action strip — **Pin/Unpin · Expand · Paste ·
//! Delete**. The strip is always there, dimmed, and lights up on hover.
//! Clicking anywhere else on the row pastes it. Resting on a row shows the
//! hover preview ([`crate::preview`]); right-click keeps the context menu.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSApplication, NSBox, NSButton, NSColor, NSEvent, NSImage,
    NSImageScaling, NSImageView, NSLineBreakMode, NSMenu, NSResponder, NSTextField,
    NSTrackingArea, NSTrackingAreaOptions, NSView,
};
use objc2_foundation::{ns_string, NSData, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::clipboard::{ClipboardItem, ContentType};
use crate::preview;
use crate::theme::{self, rect};

/// Row heights: image rows are taller so the thumbnail is readable.
pub const ROW_H_TEXT: f64 = 46.0;
pub const ROW_H_IMAGE: f64 = 64.0;
const RADIUS: f64 = 9.0;
const INSET: f64 = 7.0;
const BTN: f64 = 24.0;
const BTN_GAP: f64 = 2.0;
const ACTIONS_W: f64 = BTN * 4.0 + BTN_GAP * 3.0;
/// Action strip opacity at rest; full on hover or for the keyboard-cursor row.
const ACTIONS_REST_ALPHA: f64 = 0.38;
/// How long the pointer rests on a row before the preview appears.
const PREVIEW_DELAY: f64 = 0.35;

pub fn row_height(item: &ClipboardItem) -> f64 {
    if item.content_type == ContentType::Image && item.content_image.is_some() {
        ROW_H_IMAGE
    } else {
        ROW_H_TEXT
    }
}

/// How a row should look when it is built.
pub struct RowState<'a> {
    pub select_mode: bool,
    /// Ticked in select mode.
    pub selected: bool,
    /// Under the keyboard cursor.
    pub cursor: bool,
    /// The ranker's reason, if this row is its suggestion.
    pub suggestion: Option<&'a str>,
}

pub struct RowIvars {
    item: ClipboardItem,
    target: Retained<AnyObject>,
    select_mode: bool,
    cursor: bool,
    hovering: Cell<bool>,
    hover_bg: RefCell<Option<Retained<NSBox>>>,
    actions: RefCell<Option<Retained<NSView>>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    // SAFETY: NSView subclass with no extra invariants; ivars are main-thread only.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZnonClipRowView"]
    #[ivars = RowIvars]
    pub struct RowView;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for RowView {}

    impl RowView {
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&old);
            }
            let opts = NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            // SAFETY: owner is self, which outlives its own tracking area.
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    opts,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            *self.ivars().tracking.borrow_mut() = Some(area);
            // SAFETY: calling the superclass implementation is required by AppKit.
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hovering.set(true);
            self.set_hover_visuals(true);
            if !self.ivars().select_mode {
                // SAFETY: performSelector:withObject:afterDelay: is NSObject API.
                let _: () = unsafe {
                    msg_send![
                        self,
                        performSelector: sel!(showPreviewNow:),
                        withObject: std::ptr::null::<AnyObject>(),
                        afterDelay: PREVIEW_DELAY
                    ]
                };
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hovering.set(false);
            self.set_hover_visuals(false);
            self.cancel_pending_preview();
            preview::hide_hover();
        }

        #[unsafe(method(showPreviewNow:))]
        fn show_preview_now(&self, _sender: Option<&AnyObject>) {
            if !self.ivars().hovering.get() {
                return;
            }
            let (Some(mtm), Some(win)) = (MainThreadMarker::new(), self.window()) else {
                return;
            };
            if !win.isVisible() {
                return;
            }
            preview::show_hover(mtm, &self.ivars().item, self.screen_rect(), win.frame());
        }

        /// The first click on an inactive floater should act, not just focus.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        /// Click on the row body: paste it (or toggle it in select mode).
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            if !rect_contains(self.bounds(), p) {
                return;
            }
            self.cancel_pending_preview();
            preview::hide_hover();
            let action = if self.ivars().select_mode {
                sel!(toggleItemSelection:)
            } else {
                sel!(copyHistoryItem:)
            };
            self.send(action);
        }

        /// The row owns every click except those on its action buttons, so the
        /// title and meta labels never swallow a click.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            // SAFETY: superview is a plain getter.
            let sup = unsafe { self.superview() };
            let local = self.convertPoint_fromView(point, sup.as_deref());
            if !rect_contains(self.bounds(), local) {
                return std::ptr::null_mut();
            }
            if let Some(strip) = self.ivars().actions.borrow().as_ref() {
                if rect_contains(strip.frame(), local) {
                    // SAFETY: forwarding to NSView's own hit testing.
                    return unsafe { msg_send![super(self), hitTest: point] };
                }
            }
            (self as *const Self as *mut Self).cast()
        }

        /// Swallow mouseDown so the click becomes a mouseUp here, not a window drag.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            false
        }
    }
);

impl RowView {
    pub fn item_id(&self) -> u64 {
        self.ivars().item.id
    }


    /// This row in screen coordinates (preview anchor).
    pub fn screen_rect(&self) -> NSRect {
        let r = self.convertRect_toView(self.bounds(), None);
        self.window().map(|w| w.convertRectToScreen(r)).unwrap_or(r)
    }

    /// Fade the row out and slide it right, then call `done`: delete feedback.
    pub fn animate_removal(&self, done: Box<dyn Fn()>) {
        self.cancel_pending_preview();
        let me = self.retain();
        let mut f = self.frame();
        f.origin.x += 28.0;
        theme::animate(
            theme::T_BASE,
            move || {
                me.animator().setAlphaValue(0.0);
                me.animator().setFrame(f);
            },
            Some(done),
        );
    }

    fn send(&self, action: Sel) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: the delegate implements every action a row sends; sender is self.
        unsafe {
            app.sendAction_to_from(action, Some(&self.ivars().target), Some(self));
        }
    }

    fn cancel_pending_preview(&self) {
        // SAFETY: class method on NSObject; arguments match its signature.
        let _: () = unsafe {
            msg_send![
                objc2::class!(NSObject),
                cancelPreviousPerformRequestsWithTarget: self,
                selector: sel!(showPreviewNow:),
                object: std::ptr::null::<AnyObject>()
            ]
        };
    }

    fn set_hover_visuals(&self, on: bool) {
        if let Some(bg) = self.ivars().hover_bg.borrow().as_ref() {
            theme::fade(bg, if on { 1.0 } else { 0.0 }, theme::T_FAST);
        }
        if let Some(strip) = self.ivars().actions.borrow().as_ref() {
            let rest = if self.ivars().cursor { 1.0 } else { ACTIONS_REST_ALPHA };
            theme::fade(strip, if on { 1.0 } else { rest }, theme::T_FAST);
        }
    }
}

/// Build a row for `item`, `width` wide.
pub fn make_row(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    target: &AnyObject,
    state: &RowState,
    width: f64,
) -> Retained<RowView> {
    let h = row_height(item);
    let this = RowView::alloc(mtm).set_ivars(RowIvars {
        item: item.clone(),
        target: target.retain(),
        select_mode: state.select_mode,
        cursor: state.cursor,
        hovering: Cell::new(false),
        hover_bg: RefCell::new(None),
        actions: RefCell::new(None),
        tracking: RefCell::new(None),
    });
    // SAFETY: NSView's designated initializer.
    let row: Retained<RowView> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, width, h)] };

    // Background layers: hover wash (hidden until hover), cursor / selection glow.
    let hover_bg = theme::rounded_box(mtm, rect(0.0, 0.0, width, h), RADIUS, &theme::glass(0.06), None);
    hover_bg.setAlphaValue(0.0);
    row.addSubview(&hover_bg);
    *row.ivars().hover_bg.borrow_mut() = Some(hover_bg);

    let accent = if item.is_pinned { theme::neon_pin() } else { theme::neon() };
    if state.cursor || state.selected {
        let glow = theme::rounded_box(
            mtm,
            rect(0.0, 0.0, width, h),
            RADIUS,
            &theme::with_alpha(&accent, 0.13),
            Some((&theme::with_alpha(&accent, 0.55), 1.0)),
        );
        row.addSubview(&glow);
        let bar = theme::rounded_box(mtm, rect(1.0, 9.0, 3.0, h - 18.0), 1.5, &accent, None);
        row.addSubview(&bar);
    }

    // Leading thumbnail or type tile.
    let tile = h - 12.0;
    let tile_x = INSET + 3.0;
    let tile_y = 6.0;
    let mut placed_thumb = false;
    if !state.select_mode {
        if let Some(img) = thumbnail(item, tile) {
            let frame = theme::rounded_box(
                mtm,
                rect(tile_x, tile_y, tile, tile),
                7.0,
                &theme::glass(0.05),
                Some((&theme::glass(0.14), 1.0)),
            );
            row.addSubview(&frame);
            let iv = NSImageView::imageViewWithImage(&img, mtm);
            iv.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
            iv.setFrame(rect(tile_x + 1.0, tile_y + 1.0, tile - 2.0, tile - 2.0));
            theme::round_corners(&iv, 6.0);
            row.addSubview(&iv);
            placed_thumb = true;
        }
    }
    if !placed_thumb {
        let fill = if item.is_pinned {
            theme::with_alpha(&theme::neon_pin(), 0.14)
        } else {
            theme::glass(0.06)
        };
        let tile_box = theme::rounded_box(
            mtm,
            rect(tile_x, tile_y, tile, tile),
            7.0,
            &fill,
            Some((&theme::glass(0.08), 1.0)),
        );
        row.addSubview(&tile_box);
        let symbol_name = if state.select_mode {
            if state.selected { "checkmark.circle.fill" } else { "circle" }
        } else if state.suggestion.is_some() {
            "sparkles"
        } else {
            item.content_type.sf_symbol()
        };
        if let Some(sym) = theme::symbol(symbol_name, item.content_type.label(), 14.0) {
            let iv = NSImageView::imageViewWithImage(&sym, mtm);
            iv.setFrame(rect(tile_x, tile_y, tile, tile));
            let tint: Retained<NSColor> = if state.select_mode && state.selected {
                theme::neon()
            } else if item.is_pinned {
                theme::neon_pin()
            } else {
                theme::with_alpha(&theme::neon(), 0.85)
            };
            iv.setContentTintColor(Some(&tint));
            row.addSubview(&iv);
        }
    }

    // Title + meta.
    let text_x = tile_x + tile + 10.0;
    let right_reserve = if state.select_mode { 10.0 } else { ACTIONS_W + 12.0 };
    let text_w = (width - text_x - right_reserve).max(60.0);

    let title = NSTextField::labelWithString(&NSString::from_str(&row_title(item)), mtm);
    title.setFont(Some(&theme::ui(12.5)));
    title.setTextColor(Some(&NSColor::labelColor()));
    title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    title.setUsesSingleLineMode(true);
    title.setFrame(rect(text_x, h / 2.0 + 0.5, text_w, 17.0));
    row.addSubview(&title);

    let meta = NSTextField::labelWithString(&NSString::from_str(&row_meta(item, state.suggestion)), mtm);
    meta.setFont(Some(&theme::mono(9.5)));
    let meta_color: Retained<NSColor> = if state.suggestion.is_some() {
        theme::with_alpha(&theme::neon(), 0.9)
    } else {
        NSColor::tertiaryLabelColor()
    };
    meta.setTextColor(Some(&meta_color));
    meta.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    meta.setUsesSingleLineMode(true);
    meta.setFrame(rect(text_x, h / 2.0 - 15.0, text_w, 13.0));
    row.addSubview(&meta);

    if !state.select_mode {
        let strip = action_strip(mtm, item, target);
        strip.setFrame(rect(width - ACTIONS_W - INSET, (h - BTN) / 2.0, ACTIONS_W, BTN));
        strip.setAlphaValue(if state.cursor { 1.0 } else { ACTIONS_REST_ALPHA });
        row.addSubview(&strip);
        *row.ivars().actions.borrow_mut() = Some(strip);

        // SAFETY: setMenu: on NSResponder; the menu is retained by the view.
        unsafe { row.setMenu(Some(&context_menu(mtm, item, target))) };
    }

    if let Some(reason) = state.suggestion {
        row.setToolTip(Some(&NSString::from_str(&format!(
            "Suggested: {reason}. Press Enter to paste."
        ))));
    }
    row
}

fn action_strip(mtm: MainThreadMarker, item: &ClipboardItem, target: &AnyObject) -> Retained<NSView> {
    let strip = NSView::new(mtm);
    let pinned = item.is_pinned;
    let buttons: [(&str, &str, Sel, Retained<NSColor>); 4] = [
        (
            if pinned { "pin.fill" } else { "pin" },
            if pinned { "Unpin (⌘P)" } else { "Pin (⌘P)" },
            sel!(pinHistoryItem:),
            if pinned { theme::neon_pin() } else { NSColor::secondaryLabelColor() },
        ),
        (
            "arrow.up.left.and.arrow.down.right",
            "Expand preview (⌘E)",
            sel!(expandHistoryItem:),
            NSColor::secondaryLabelColor(),
        ),
        (
            "arrow.turn.down.left",
            "Paste (Enter)",
            sel!(copyHistoryItem:),
            theme::neon(),
        ),
        (
            "trash",
            "Delete (⌘⌫)",
            sel!(deleteHistoryItem:),
            theme::with_alpha(&theme::neon_danger(), 0.9),
        ),
    ];
    for (i, (symbol, tip, action, tint)) in buttons.into_iter().enumerate() {
        let img = theme::symbol(symbol, tip, 12.0);
        let b = match img {
            Some(img) => unsafe {
                NSButton::buttonWithImage_target_action(&img, Some(target), Some(action), mtm)
            },
            None => unsafe {
                NSButton::buttonWithTitle_target_action(&NSString::from_str(tip), Some(target), Some(action), mtm)
            },
        };
        b.setBordered(false);
        b.setContentTintColor(Some(&tint));
        b.setTag(item.id as isize);
        b.setToolTip(Some(&NSString::from_str(tip)));
        b.setFrame(rect(i as f64 * (BTN + BTN_GAP), 0.0, BTN, BTN));
        strip.addSubview(&b);
    }
    strip
}

fn context_menu(mtm: MainThreadMarker, item: &ClipboardItem, target: &AnyObject) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let entries: [(&NSString, Sel); 4] = [
        (ns_string!("Paste"), sel!(copyHistoryItem:)),
        (
            if item.is_pinned { ns_string!("Unpin") } else { ns_string!("Pin") },
            sel!(pinHistoryItem:),
        ),
        (ns_string!("Expand preview"), sel!(expandHistoryItem:)),
        (ns_string!("Delete"), sel!(deleteHistoryItem:)),
    ];
    for (title, action) in entries {
        let mi = unsafe { menu.addItemWithTitle_action_keyEquivalent(title, Some(action), ns_string!("")) };
        mi.setTag(item.id as isize);
        unsafe { mi.setTarget(Some(target)) };
    }
    menu
}

fn row_title(item: &ClipboardItem) -> String {
    if item.content_type == ContentType::Image {
        return "Image".into();
    }
    // First non-empty line, whitespace collapsed.
    let src = item
        .content_text
        .as_deref()
        .or(item.content_url.as_deref())
        .unwrap_or(item.preview.as_str());
    let line = src.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(item.preview.as_str());
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn row_meta(item: &ClipboardItem, suggestion: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if suggestion.is_some() {
        parts.push("✦ suggested".into());
    }
    let rel = crate::status_item::format_relative_time(&item.created_at);
    if !rel.is_empty() {
        parts.push(rel);
    }
    parts.push(item.content_type.label().to_string());
    if let Some(t) = item.content_text.as_deref() {
        let n = t.chars().count();
        parts.push(if n >= 1000 { format!("{:.1}k chars", n as f64 / 1000.0) } else { format!("{n} chars") });
    }
    if let Some(app) = short_app_name(item.source_app_bundle_id.as_deref()) {
        parts.push(app);
    }
    parts.join(" · ")
}

/// "com.google.Chrome" → "chrome", "com.anthropic.claudefordesktop" → "claude".
/// Short source tag for meta lines.
pub fn short_app_name(bundle_id: Option<&str>) -> Option<String> {
    let mut name = bundle_id?.rsplit('.').next()?.to_lowercase();
    for suffix in ["fordesktop", "desktop", "-app", "app"] {
        if name.len() > suffix.len() + 2 && name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    if name.is_empty() {
        return None;
    }
    Some(name.chars().take(12).collect())
}

/// Thumbnail sized to fit a `tile`-point square.
fn thumbnail(item: &ClipboardItem, tile: f64) -> Option<Retained<NSImage>> {
    if item.content_type != ContentType::Image {
        return None;
    }
    let data = NSData::with_bytes(item.content_image.as_deref()?);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    let s = image.size();
    if s.width <= 0.0 || s.height <= 0.0 {
        return None;
    }
    let scale = (tile / s.width).min(tile / s.height);
    image.setSize(NSSize::new((s.width * scale).max(1.0), (s.height * scale).max(1.0)));
    Some(image)
}

fn rect_contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x && p.y >= r.origin.y && p.x < r.origin.x + r.size.width && p.y < r.origin.y + r.size.height
}
