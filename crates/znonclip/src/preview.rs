//! Hover / expand preview beside the floater.
//!
//! * **Hover**: resting on a row for a moment shows a compact peek: a larger
//!   image, or the first lines of the text. It ignores the mouse and vanishes
//!   when the pointer leaves the row.
//! * **Expanded**: the row's Expand button (or ⌘E) opens a bigger, scrollable,
//!   selectable view that stays until Expand is pressed again, Esc, or the
//!   floater closes. While expanded, hover peeks are suppressed.
//! * **Receipt**: the row's clock-in-chain button (or ⌘I) shows the item's
//!   content SHA-256, what the hash covers, and when it was first and last
//!   copied, with Copy buttons. Sticky like Expanded.
//!
//! One borderless, non-activating panel is reused for both; it lives in a
//! main-thread `thread_local` because rows and the app delegate both drive it.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAppearanceCustomization, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton,
    NSColor, NSFloatingWindowLevel, NSImage, NSImageScaling, NSImageView, NSLineBreakMode,
    NSPanel, NSScreen, NSTextField, NSTextView, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSData, NSPoint, NSRect, NSSize, NSString};

use crate::clipboard::{ClipboardItem, ContentType};
use crate::storage::Receipt;
use crate::theme::{self, rect};

const PAD: f64 = 12.0;
const RADIUS: f64 = 12.0;
const CAPTION_H: f64 = 16.0;
/// Gap between the floater and the preview.
const GAP: f64 = 10.0;

const HOVER_IMAGE_MAX: f64 = 260.0;
const HOVER_TEXT_W: f64 = 320.0;
const HOVER_TEXT_LINES: isize = 14;
const HOVER_TEXT_CHARS: usize = 1200;

const EXPANDED_IMAGE_MAX: f64 = 560.0;
const EXPANDED_W: f64 = 480.0;
const EXPANDED_H: f64 = 400.0;
const EXPANDED_TEXT_CHARS: usize = 200_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Hover,
    Expanded,
    Receipt,
}

struct Preview {
    panel: Retained<NSPanel>,
    /// Which item and mode is on screen, if any.
    showing: Option<(u64, Mode)>,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

/// Show the compact hover peek for `item` next to `anchor` (a row, in screen
/// coordinates). No-op while an expanded preview is open.
pub fn show_hover(mtm: MainThreadMarker, item: &ClipboardItem, anchor: NSRect, floater: NSRect) {
    with_preview(mtm, |p| {
        if matches!(p.showing, Some((_, Mode::Expanded | Mode::Receipt))) {
            return;
        }
        present(mtm, p, item, Mode::Hover, anchor, floater, None);
    });
}

/// Hide the hover peek (leaves an expanded preview alone).
pub fn hide_hover() {
    PREVIEW.with(|cell| {
        if let Some(p) = cell.borrow_mut().as_mut() {
            if matches!(p.showing, Some((_, Mode::Hover))) {
                dismiss(p);
            }
        }
    });
}

/// Toggle the expanded preview for `item`. Returns true if it is now open.
pub fn toggle_expanded(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    anchor: NSRect,
    floater: NSRect,
    close_target: &AnyObject,
) -> bool {
    let mut open = false;
    with_preview(mtm, |p| {
        if p.showing == Some((item.id, Mode::Expanded)) {
            dismiss(p);
        } else {
            present(mtm, p, item, Mode::Expanded, anchor, floater, Some(close_target));
            open = true;
        }
    });
    open
}

/// Swap an open expanded preview to `item` (keyboard cursor moved).
pub fn follow_if_expanded(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    anchor: NSRect,
    floater: NSRect,
    close_target: &AnyObject,
) {
    with_preview(mtm, |p| {
        if matches!(p.showing, Some((id, Mode::Expanded)) if id != item.id) {
            present(mtm, p, item, Mode::Expanded, anchor, floater, Some(close_target));
        }
    });
}

/// True while a sticky panel (expanded preview or receipt) is open.
pub fn is_expanded() -> bool {
    PREVIEW.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|p| matches!(p.showing, Some((_, Mode::Expanded | Mode::Receipt))))
    })
}

/// Toggle the receipt panel for `item`. Returns true if it is now open.
pub fn toggle_receipt(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    receipt: &Receipt,
    anchor: NSRect,
    floater: NSRect,
    target: &AnyObject,
) -> bool {
    let mut open = false;
    with_preview(mtm, |p| {
        if p.showing == Some((item.id, Mode::Receipt)) {
            dismiss(p);
        } else {
            let (content, size) = build_receipt(mtm, item, receipt, target);
            present_built(mtm, p, item.id, Mode::Receipt, content, size, anchor, floater);
            open = true;
        }
    });
    open
}

/// The text "Copy receipt" puts on the pasteboard.
pub fn receipt_text(receipt: &Receipt) -> String {
    format!(
        "ZnonClip receipt\nsha256: {}\ncovers: {}\nfirst copied: {}\nlast copied: {}\n",
        receipt.sha256.as_deref().unwrap_or("not recorded"),
        receipt.basis.as_deref().unwrap_or("unknown"),
        receipt.first_copied_at.as_deref().unwrap_or("not recorded (captured before receipts)"),
        receipt.last_copied_at,
    )
}

/// Close whatever preview is showing.
pub fn close() {
    PREVIEW.with(|cell| {
        if let Some(p) = cell.borrow_mut().as_mut() {
            dismiss(p);
        }
    });
}

// ── internals ───────────────────────────────────────────────────────────────

fn with_preview(mtm: MainThreadMarker, f: impl FnOnce(&mut Preview)) {
    PREVIEW.with(|cell| {
        let mut slot = cell.borrow_mut();
        let p = slot.get_or_insert_with(|| Preview {
            panel: make_panel(mtm),
            showing: None,
        });
        f(p);
    });
}

fn make_panel(mtm: MainThreadMarker) -> Retained<NSPanel> {
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect(0.0, 0.0, 300.0, 200.0),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: kept alive by the thread_local for the life of the app.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    panel.setHidesOnDeactivate(false);
    panel.setFloatingPanel(true);
    panel.setLevel(NSFloatingWindowLevel + 1);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setAppearance(theme::dark_appearance().as_deref());
    panel
}

fn dismiss(p: &mut Preview) {
    p.showing = None;
    let panel = p.panel.clone();
    let panel2 = p.panel.clone();
    theme::animate(
        theme::T_FAST,
        move || panel.animator().setAlphaValue(0.0),
        Some(Box::new(move || {
            // A newer present() may have revived it during the fade.
            if panel2.alphaValue() < 0.01 {
                panel2.orderOut(None);
            }
        })),
    );
}

fn present(
    mtm: MainThreadMarker,
    p: &mut Preview,
    item: &ClipboardItem,
    mode: Mode,
    anchor: NSRect,
    floater: NSRect,
    close_target: Option<&AnyObject>,
) {
    let (content, size) = build_content(mtm, item, mode == Mode::Expanded, close_target);
    present_built(mtm, p, item.id, mode, content, size, anchor, floater);
}

fn present_built(
    mtm: MainThreadMarker,
    p: &mut Preview,
    id: u64,
    mode: Mode,
    content: Retained<NSView>,
    size: NSSize,
    anchor: NSRect,
    floater: NSRect,
) {
    let expanded = mode != Mode::Hover;

    let glass = NSVisualEffectView::new(mtm);
    glass.setMaterial(NSVisualEffectMaterial::HUDWindow);
    glass.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    glass.setState(NSVisualEffectState::Active);
    glass.setFrame(rect(0.0, 0.0, size.width, size.height));
    theme::round_corners(&glass, RADIUS);
    let rim = theme::rounded_box(
        mtm,
        rect(0.0, 0.0, size.width, size.height),
        RADIUS,
        &NSColor::clearColor(),
        Some((&theme::with_alpha(&theme::neon(), if expanded { 0.9 } else { 0.65 }), 1.2)),
    );
    theme::glow(&rim, &theme::neon(), 8.0);
    rim.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    let tint = theme::rounded_box(
        mtm,
        rect(0.0, 0.0, size.width, size.height),
        0.0,
        &NSColor::colorWithWhite_alpha(0.04, 0.55),
        None,
    );
    glass.addSubview(&tint);
    glass.addSubview(&content);
    glass.addSubview(&rim);

    // ⌘+/⌘− zoom: the panel is scaled, its content keeps logical coordinates.
    let scale = theme::ui_scale();
    let logical = size;
    let size = NSSize::new(logical.width * scale, logical.height * scale);
    glass.setFrame(rect(0.0, 0.0, size.width, size.height));
    glass.setBoundsSize(logical);
    theme::round_corners(&glass, RADIUS * scale);
    let frame = place(size, anchor, floater);
    let was_visible = p.panel.isVisible() && p.panel.alphaValue() > 0.5;
    p.panel.setIgnoresMouseEvents(!expanded);
    p.panel.setContentView(Some(&glass));
    p.panel.setFrame_display(frame, true);
    p.showing = Some((id, mode));

    if !was_visible {
        p.panel.setAlphaValue(0.0);
    }
    p.panel.orderFrontRegardless();
    let panel = p.panel.clone();
    theme::animate(theme::T_FAST, move || panel.animator().setAlphaValue(1.0), None);
}

/// Beside the floater (left if there is room, else right), vertically centred
/// on the anchor row, clamped to the screen.
fn place(size: NSSize, anchor: NSRect, floater: NSRect) -> NSRect {
    let visible = screen_containing(floater).unwrap_or(floater);
    let left_x = floater.origin.x - GAP - size.width;
    let right_x = floater.origin.x + floater.size.width + GAP;
    let x = if left_x >= visible.origin.x {
        left_x
    } else if right_x + size.width <= visible.origin.x + visible.size.width {
        right_x
    } else {
        // No room either side: overlap the floater's left edge.
        visible.origin.x.max(floater.origin.x - size.width / 2.0)
    };
    let mid = anchor.origin.y + anchor.size.height / 2.0;
    let min_y = visible.origin.y + 8.0;
    let max_y = visible.origin.y + visible.size.height - size.height - 8.0;
    let y = (mid - size.height / 2.0).clamp(min_y, max_y.max(min_y));
    NSRect::new(NSPoint::new(x, y), size)
}

fn screen_containing(r: NSRect) -> Option<NSRect> {
    let mtm = MainThreadMarker::new()?;
    let cx = r.origin.x + r.size.width / 2.0;
    let cy = r.origin.y + r.size.height / 2.0;
    for screen in NSScreen::screens(mtm).iter() {
        let f = screen.frame();
        if cx >= f.origin.x
            && cx < f.origin.x + f.size.width
            && cy >= f.origin.y
            && cy < f.origin.y + f.size.height
        {
            return Some(screen.visibleFrame());
        }
    }
    NSScreen::mainScreen(mtm).map(|s| s.visibleFrame())
}

/// Build the preview body; returns the view and the panel size it needs.
fn build_content(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    expanded: bool,
    close_target: Option<&AnyObject>,
) -> (Retained<NSView>, NSSize) {
    let root = NSView::new(mtm);

    if let Some(image) = decode_image(item) {
        let max = if expanded { EXPANDED_IMAGE_MAX } else { HOVER_IMAGE_MAX };
        let pt = image.size();
        // Fit inside `max`; small captures may grow (up to 2x) but never below 140 pt.
        let fit = (max / pt.width).min(max / pt.height);
        let grow_floor = (140.0 / pt.width.max(pt.height)).max(1.0);
        let scale = fit.min(grow_floor.max(if expanded { 2.0 } else { 1.0 }));
        let w = (pt.width * scale).round().max(40.0);
        let h = (pt.height * scale).round().max(40.0);
        let size = NSSize::new(w + PAD * 2.0, h + PAD * 2.0 + CAPTION_H + 4.0);
        root.setFrame(rect(0.0, 0.0, size.width, size.height));

        // Hover: caption below the image. Expanded: caption (and close) above it.
        let img_y = if expanded { PAD } else { PAD + CAPTION_H + 4.0 };
        let frame_box = theme::rounded_box(
            mtm,
            rect(PAD, img_y, w, h),
            8.0,
            &theme::glass(0.04),
            Some((&theme::glass(0.10), 1.0)),
        );
        root.addSubview(&frame_box);
        let view = NSImageView::imageViewWithImage(&image, mtm);
        view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        view.setFrame(rect(PAD, img_y, w, h));
        theme::round_corners(&view, 8.0);
        root.addSubview(&view);

        add_caption(mtm, &root, &caption(item, None), size.width, expanded, close_target);
        return (root, size);
    }

    let text = full_text(item);
    let chars = text.chars().count();
    let lines = text.lines().count().max(1);
    let codeish = looks_like_code(&text);
    let font = if codeish { theme::mono(11.5) } else { theme::ui(12.5) };
    let stats = format!("{chars} chars · {lines} line{}", if lines == 1 { "" } else { "s" });

    if expanded {
        let size = NSSize::new(EXPANDED_W, EXPANDED_H);
        root.setFrame(rect(0.0, 0.0, size.width, size.height));
        let scroll = NSTextView::scrollableTextView(mtm);
        let body_h = size.height - PAD * 2.0 - CAPTION_H - 6.0;
        scroll.setFrame(rect(PAD, PAD, size.width - PAD * 2.0, body_h));
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        if let Some(doc) = scroll.documentView() {
            if let Some(tv) = doc.downcast_ref::<NSTextView>() {
                let shown: String = text.chars().take(EXPANDED_TEXT_CHARS).collect();
                tv.setString(&NSString::from_str(&shown));
                tv.setEditable(false);
                tv.setSelectable(true);
                tv.setDrawsBackground(false);
                tv.setFont(Some(&font));
                tv.setTextColor(Some(&NSColor::labelColor()));
            }
        }
        root.addSubview(&scroll);
        add_caption(
            mtm,
            &root,
            &caption(item, Some(&stats)),
            size.width,
            true,
            close_target,
        );
        return (root, size);
    }

    let shown = collapse_blank_lines(&clip_chars(text.trim(), HOVER_TEXT_CHARS));
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(&shown), mtm);
    label.setFont(Some(&font));
    label.setTextColor(Some(&NSColor::labelColor()));
    label.setMaximumNumberOfLines(HOVER_TEXT_LINES);
    label.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    if let Some(cell) = label.cell() {
        cell.setTruncatesLastVisibleLine(true);
    }
    label.setPreferredMaxLayoutWidth(HOVER_TEXT_W);
    let fit = label.fittingSize();
    let body_w = HOVER_TEXT_W.min(fit.width.max(160.0));
    let body_h = fit.height.clamp(18.0, 260.0);
    label.setFrame(rect(PAD, PAD + CAPTION_H + 6.0, body_w, body_h));
    let size = NSSize::new(body_w + PAD * 2.0, body_h + PAD * 2.0 + CAPTION_H + 6.0);
    root.setFrame(rect(0.0, 0.0, size.width, size.height));
    root.addSubview(&label);
    add_caption(mtm, &root, &caption(item, Some(&stats)), size.width, false, None);
    (root, size)
}

/// "2026-10-07T14:03:22.123Z" → "2026-10-07 14:03:22 UTC".
fn readable_utc(iso: &str) -> String {
    match (iso.get(..10), iso.get(11..19)) {
        (Some(d), Some(t)) if iso.ends_with('Z') => format!("{d} {t} UTC"),
        _ => iso.to_string(),
    }
}

fn build_receipt(
    mtm: MainThreadMarker,
    item: &ClipboardItem,
    receipt: &Receipt,
    target: &AnyObject,
) -> (Retained<NSView>, NSSize) {
    const W: f64 = 404.0;
    const ROW: f64 = 17.0;
    const BTN_H: f64 = 24.0;
    let root = NSView::new(mtm);
    let sha = receipt.sha256.as_deref().unwrap_or("not recorded");
    // 64 hex chars as two lines of 32, so the hash never truncates.
    let sha_shown = if sha.len() == 64 { format!("{}\n{}", &sha[..32], &sha[32..]) } else { sha.to_string() };
    let facts: [(&str, String); 3] = [
        ("covers", receipt.basis.clone().unwrap_or_else(|| "unknown".into())),
        (
            "first copied",
            receipt
                .first_copied_at
                .as_deref()
                .map(readable_utc)
                .unwrap_or_else(|| "before receipts existed".into()),
        ),
        ("last copied", readable_utc(&receipt.last_copied_at)),
    ];
    let hash_h = 34.0;
    let h = PAD + BTN_H + 10.0 + ROW * facts.len() as f64 + 8.0 + hash_h + 6.0 + CAPTION_H + PAD;
    let size = NSSize::new(W, h);
    root.setFrame(rect(0.0, 0.0, W, h));

    // Buttons along the bottom.
    let buttons = [("Copy hash", sel!(copyReceiptHash:)), ("Copy receipt", sel!(copyReceiptText:))];
    let mut x = PAD;
    for (title, action) in buttons {
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm)
        };
        b.setTag(item.id as isize);
        b.setContentTintColor(Some(&theme::neon()));
        let w = b.fittingSize().width.max(96.0);
        b.setFrame(rect(x, PAD, w, BTN_H));
        root.addSubview(&b);
        x += w + 8.0;
    }

    // Facts, bottom-up.
    let mut y = PAD + BTN_H + 10.0;
    for (label, value) in facts.iter().rev() {
        let k = NSTextField::labelWithString(&NSString::from_str(label), mtm);
        k.setFont(Some(&theme::mono(10.0)));
        k.setTextColor(Some(&NSColor::secondaryLabelColor()));
        k.setFrame(rect(PAD, y, 84.0, ROW));
        root.addSubview(&k);
        let v = NSTextField::labelWithString(&NSString::from_str(value), mtm);
        v.setFont(Some(&theme::mono(11.0)));
        v.setTextColor(Some(&NSColor::labelColor()));
        v.setSelectable(true);
        v.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        v.setFrame(rect(PAD + 88.0, y, W - PAD * 2.0 - 88.0, ROW));
        root.addSubview(&v);
        y += ROW;
    }

    // The hash, large and selectable.
    y += 8.0;
    let hash = NSTextField::wrappingLabelWithString(&NSString::from_str(&sha_shown), mtm);
    hash.setFont(Some(&theme::mono(12.5)));
    hash.setTextColor(Some(&theme::neon()));
    hash.setSelectable(true);
    hash.setFrame(rect(PAD, y, W - PAD * 2.0, hash_h));
    theme::glow(&hash, &theme::neon(), 4.0);
    root.addSubview(&hash);

    add_caption(mtm, &root, &format!("RECEIPT · SHA-256 · {}", item.content_type.label().to_uppercase()), W, true, Some(target));
    (root, size)
}

fn add_caption(
    mtm: MainThreadMarker,
    root: &NSView,
    text: &str,
    width: f64,
    expanded: bool,
    close_target: Option<&AnyObject>,
) {
    let top = root.frame().size.height;
    // Expanded: caption across the top with a close button; hover: along the bottom.
    let y = if expanded { top - PAD - CAPTION_H + 2.0 } else { PAD - 2.0 };
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&theme::mono(10.0)));
    label.setTextColor(Some(&theme::with_alpha(&theme::neon(), 0.85)));
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    label.setFrame(rect(PAD, y, width - PAD * 2.0 - if expanded { 24.0 } else { 0.0 }, CAPTION_H));
    root.addSubview(&label);

    if let (true, Some(target)) = (expanded, close_target) {
        if let Some(img) = theme::symbol("xmark.circle.fill", "Close preview", 13.0) {
            let close = unsafe {
                NSButton::buttonWithImage_target_action(
                    &img,
                    Some(target),
                    Some(sel!(collapsePreview:)),
                    mtm,
                )
            };
            close.setBordered(false);
            close.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            close.setFrame(rect(width - PAD - 20.0, y - 2.0, 20.0, 20.0));
            close.setToolTip(Some(&NSString::from_str("Close preview (Esc)")));
            root.addSubview(&close);
        }
    }
}

fn caption(item: &ClipboardItem, stats: Option<&str>) -> String {
    let mut parts = vec![item.content_type.label().to_string()];
    if let Some(s) = stats {
        parts.push(s.to_string());
    }
    if let Some(app) = crate::row::short_app_name(item.source_app_bundle_id.as_deref()) {
        parts.push(app);
    }
    if item.is_pinned {
        parts.push("pinned".into());
    }
    parts.join(" · ").to_uppercase()
}

fn decode_image(item: &ClipboardItem) -> Option<Retained<NSImage>> {
    if item.content_type != ContentType::Image {
        return None;
    }
    let data = NSData::with_bytes(item.content_image.as_deref()?);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    // Use pixel dimensions: the stored bitmap is pixel-bounded, not point-bounded.
    let reps = image.representations();
    if let Some(rep) = reps.iter().next() {
        let (pw, ph) = (rep.pixelsWide() as f64, rep.pixelsHigh() as f64);
        if pw > 0.0 && ph > 0.0 {
            // Show 2 px per point so a 512 px capture is a sharp 256 pt preview.
            image.setSize(NSSize::new(pw / 2.0, ph / 2.0));
        }
    }
    let s = image.size();
    (s.width > 0.0 && s.height > 0.0).then_some(image)
}

fn full_text(item: &ClipboardItem) -> String {
    if let Some(t) = item.content_text.as_deref().filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    if let Some(u) = item.content_url.as_deref() {
        return u.to_string();
    }
    if let Some(paths) = item.content_file_paths.as_ref() {
        return paths.join("\n");
    }
    item.preview.clone()
}

fn looks_like_code(text: &str) -> bool {
    let sample: String = text.chars().take(600).collect();
    let signals = ["{", "}", ";", "=>", "fn ", "def ", "const ", "import ", "</", "$ ", "    "];
    signals.iter().filter(|s| sample.contains(*s)).count() >= 2
}

fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// The preview body for `item`, as it would appear, for headless snapshots.
pub fn snapshot_view(mtm: MainThreadMarker, item: &ClipboardItem, expanded: bool) -> Retained<NSView> {
    build_content(mtm, item, expanded, None).0
}

/// The receipt panel on its dark backing, as it looks on screen.
pub fn snapshot_receipt(mtm: MainThreadMarker, item: &ClipboardItem, receipt: &Receipt, target: &AnyObject) -> Retained<NSView> {
    let (content, size) = build_receipt(mtm, item, receipt, target);
    let back = theme::rounded_box(
        mtm,
        rect(0.0, 0.0, size.width, size.height),
        RADIUS,
        &NSColor::colorWithWhite_alpha(0.11, 1.0),
        Some((&theme::with_alpha(&theme::neon(), 0.9), 1.2)),
    );
    back.setAppearance(theme::dark_appearance().as_deref());
    back.addSubview(&content);
    Retained::into_super(back)
}

/// At most one blank line in a row: pasted docs often open with several.
fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blanks = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}
