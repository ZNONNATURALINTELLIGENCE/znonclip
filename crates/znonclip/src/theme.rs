//! Visual language for the floater and its preview: dark glass, a neon-cyan
//! accent, monospaced meta text, and short animations for feedback.
//!
//! Everything here is a small helper so `status_item`, `row` and `preview`
//! draw from one palette and one set of timings.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker, Message};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSAppearance, NSAppearanceNameDarkAqua,
    NSBox, NSBoxType, NSColor, NSFont, NSFontWeightRegular,
    NSFontWeightSemibold, NSImage, NSImageSymbolConfiguration, NSImageSymbolScale, NSTitlePosition,
    NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

// ── Palette ─────────────────────────────────────────────────────────────────

/// Primary accent: neon cyan.
pub fn neon() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.30, 0.86, 1.0, 1.0)
}

/// Pinned accent: soft violet.
pub fn neon_pin() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.78, 0.52, 1.0, 1.0)
}

/// Destructive accent.
pub fn neon_danger() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.36, 0.42, 1.0)
}

pub fn with_alpha(c: &NSColor, a: f64) -> Retained<NSColor> {
    c.colorWithAlphaComponent(a)
}

/// White at `a` (glass highlights on a dark surface).
pub fn glass(a: f64) -> Retained<NSColor> {
    NSColor::colorWithWhite_alpha(1.0, a)
}

/// The floater and preview always render dark: the glass look depends on it.
pub fn dark_appearance() -> Option<Retained<NSAppearance>> {
    NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
}

// ── Type ────────────────────────────────────────────────────────────────────

pub fn ui(size: f64) -> Retained<NSFont> {
    unsafe { NSFont::systemFontOfSize_weight(size, NSFontWeightRegular) }
}


pub fn mono(size: f64) -> Retained<NSFont> {
    unsafe { NSFont::monospacedSystemFontOfSize_weight(size, NSFontWeightRegular) }
}

pub fn mono_semibold(size: f64) -> Retained<NSFont> {
    unsafe { NSFont::monospacedSystemFontOfSize_weight(size, NSFontWeightSemibold) }
}

// ── Shapes ──────────────────────────────────────────────────────────────────

/// A rounded rectangle with a fill and an optional hairline border.
pub fn rounded_box(
    mtm: MainThreadMarker,
    frame: NSRect,
    radius: f64,
    fill: &NSColor,
    border: Option<(&NSColor, f64)>,
) -> Retained<NSBox> {
    let b = NSBox::new(mtm);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setCornerRadius(radius);
    b.setFillColor(fill);
    match border {
        Some((color, width)) => {
            b.setBorderColor(color);
            b.setBorderWidth(width);
        }
        None => b.setBorderWidth(0.0),
    }
    b.setFrame(frame);
    b
}

/// Clip a view's layer to rounded corners.
pub fn round_corners(view: &NSView, radius: f64) {
    view.setWantsLayer(true);
    // SAFETY: a layer-backed NSView always has a CALayer; both setters exist on CALayer.
    unsafe {
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }
}

/// SF Symbol at `point_size`, rendered as a template.
pub fn symbol(name: &str, a11y: &str, point_size: f64) -> Option<Retained<NSImage>> {
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

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

// ── Motion ──────────────────────────────────────────────────────────────────

/// Quick: hover and press feedback.
pub const T_FAST: f64 = 0.12;
/// Standard: open, close, row removal.
pub const T_BASE: f64 = 0.18;
/// Slow: a flash that fades out after an action.
pub const T_FLASH: f64 = 0.55;

/// Run `changes` inside an animation group of `duration` seconds, then `done`.
pub fn animate(duration: f64, changes: impl Fn() + 'static, done: Option<Box<dyn Fn()>>) {
    let changes = RcBlock::new(move |ctx: std::ptr::NonNull<NSAnimationContext>| {
        // SAFETY: AppKit passes a valid context for the duration of the block.
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(duration);
        ctx.setAllowsImplicitAnimation(true);
        changes();
    });
    let done = done.map(RcBlock::new);
    NSAnimationContext::runAnimationGroup_completionHandler(&changes, done.as_deref());
}

/// Fade a view's alpha to `to`.
pub fn fade(view: &NSView, to: f64, duration: f64) {
    let v = view.retain();
    animate(duration, move || v.animator().setAlphaValue(to), None);
}

/// Lay an accent wash over `view` and let it fade out: the "it worked" signal.
pub fn flash(mtm: MainThreadMarker, view: &NSView, color: &NSColor, radius: f64) {
    let overlay = rounded_box(
        mtm,
        view.bounds(),
        radius,
        &with_alpha(color, 0.30),
        Some((&with_alpha(color, 0.9), 1.0)),
    );
    view.addSubview(&overlay);
    let o = overlay.clone();
    let o2 = overlay.clone();
    animate(
        T_FLASH,
        move || o.animator().setAlphaValue(0.0),
        Some(Box::new(move || o2.removeFromSuperview())),
    );
}


// ── Snapshot (headless visual check) ────────────────────────────────────────

/// Render `view` to a PNG at `path` without touching the screen: works with no
/// Screen Recording permission. Behind-window glass renders transparent.
pub fn save_png(view: &NSView, path: &std::path::Path) -> std::io::Result<()> {
    use objc2_app_kit::NSBitmapImageFileType;
    use objc2_foundation::NSDictionary;
    let bounds = view.bounds();
    let rep = view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or_else(|| std::io::Error::other("no bitmap rep"))?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let props = NSDictionary::<NSString, AnyObject>::new();
    // SAFETY: PNG with empty properties is a valid request.
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }
        .ok_or_else(|| std::io::Error::other("PNG encode failed"))?;
    std::fs::write(path, data.to_vec())
}
