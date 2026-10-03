//! Clipboard polling and content extraction via `NSPasteboard`.
//!
//! Polls `changeCount` on a timer. Extracts text, RTF, HTML, images
//! (thumbnail), file paths, and URLs. Persistence is handled by [`crate::storage`].

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, info, warn};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSPasteboard, NSPasteboardTypeFileURL,
    NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString,
    NSPasteboardTypeTIFF, NSPasteboardTypeURL, NSWorkspace,
};
use objc2_foundation::{NSArray, NSData, NSPoint, NSRect, NSSize, NSString};
use sha2::{Digest, Sha256};

use crate::privacy::{self, PrivacyMarkers};

/// Default max items kept in the in-memory history ring buffer.
pub const DEFAULT_HISTORY_LIMIT: usize = 100;

/// Result of a single clipboard poll tick.
#[derive(Debug)]
pub enum PollResult {
    /// `changeCount` unchanged — nothing to do.
    NoChange,
    /// Change detected but privacy markers present; **no** content was read.
    SkippedPrivate(PrivacyMarkers),
    /// Change detected but no usable payload types.
    EmptyOrUnsupported,
    /// New item extracted (caller must persist / update UI).
    Captured(ClipboardItem),
}

/// Maximum thumbnail edge length in pixels.
const THUMB_MAX: f64 = 256.0;

/// Kind of clipboard payload (maps to storage `content_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentType {
    Text,
    Rtf,
    Html,
    Image,
    File,
    Url,
}

impl ContentType {
    /// Short label used in the popover list (type indicator).
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "TXT",
            Self::Rtf => "RTF",
            Self::Html => "HTML",
            Self::Image => "IMG",
            Self::File => "FILE",
            Self::Url => "URL",
        }
    }

    /// SF Symbol name for list rows (system symbols, enterprise-style).
    pub fn sf_symbol(self) -> &'static str {
        match self {
            Self::Text => "text.alignleft",
            Self::Rtf => "doc.richtext",
            Self::Html => "chevron.left.forwardslash.chevron.right",
            Self::Image => "photo",
            Self::File => "doc",
            Self::Url => "link",
        }
    }

    /// Stable string stored in SQLite `content_type` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Rtf => "rtf",
            Self::Html => "html",
            Self::Image => "image",
            Self::File => "file",
            Self::Url => "url",
        }
    }

    /// Parse a stored content-type string (falls back to Text).
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "text" => Self::Text,
            "rtf" => Self::Rtf,
            "html" => Self::Html,
            "image" => Self::Image,
            "file" => Self::File,
            "url" => Self::Url,
            _ => Self::Text,
        }
    }
}

/// One captured clipboard entry (in-memory cache + DB row shape).
#[derive(Debug, Clone)]
pub struct ClipboardItem {
    pub id: u64,
    pub content_type: ContentType,
    pub content_text: Option<String>,
    pub content_rtf: Option<Vec<u8>>,
    pub content_html: Option<String>,
    /// JPEG/TIFF thumbnail bytes (max 256×256), never full-res originals.
    pub content_image: Option<Vec<u8>>,
    pub content_file_paths: Option<Vec<String>>,
    pub content_url: Option<String>,
    pub source_app_bundle_id: Option<String>,
    pub is_pinned: bool,
    /// ISO 8601 timestamp (`…Z`) matching the SQLite `created_at` column.
    pub created_at: String,
    /// SHA-256 hex of primary content (deduplication key).
    pub hash: String,
    /// One-line preview for the popover list.
    pub preview: String,
}

impl ClipboardItem {
    /// Build a one-line UI preview from available fields.
    pub fn make_preview(
        content_type: ContentType,
        text: Option<&str>,
        html: Option<&str>,
        url: Option<&str>,
        file_paths: Option<&[String]>,
        has_image: bool,
        has_rtf: bool,
    ) -> String {
        match content_type {
            ContentType::Text => truncate_preview(text.unwrap_or("")),
            ContentType::Rtf => {
                if let Some(t) = text.filter(|s| !s.is_empty()) {
                    truncate_preview(t)
                } else if has_rtf {
                    "[Rich Text]".to_string()
                } else {
                    "[Rich Text]".to_string()
                }
            }
            ContentType::Html => truncate_preview(html.or(text).unwrap_or("[HTML]")),
            ContentType::Image => {
                if has_image {
                    "[Image]".to_string()
                } else {
                    "[Image]".to_string()
                }
            }
            ContentType::File => file_preview(file_paths.unwrap_or(&[])),
            ContentType::Url => truncate_preview(url.or(text).unwrap_or("[URL]")),
        }
    }

    /// SHA-256 over the primary payload for consecutive-duplicate detection.
    pub fn compute_hash(
        content_type: ContentType,
        text: Option<&str>,
        rtf: Option<&[u8]>,
        html: Option<&str>,
        image: Option<&[u8]>,
        file_paths: Option<&[String]>,
        url: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content_type.as_str().as_bytes());
        hasher.update([0xff]);

        match content_type {
            ContentType::Text => {
                if let Some(t) = text {
                    hasher.update(t.as_bytes());
                }
            }
            ContentType::Rtf => {
                if let Some(b) = rtf {
                    hasher.update(b);
                } else if let Some(t) = text {
                    hasher.update(t.as_bytes());
                }
            }
            ContentType::Html => {
                if let Some(h) = html {
                    hasher.update(h.as_bytes());
                } else if let Some(t) = text {
                    hasher.update(t.as_bytes());
                }
            }
            ContentType::Image => {
                if let Some(b) = image {
                    hasher.update(b);
                }
            }
            ContentType::File => {
                if let Some(paths) = file_paths {
                    let mut sorted = paths.to_vec();
                    sorted.sort();
                    for p in sorted {
                        hasher.update(p.as_bytes());
                        hasher.update([0x00]);
                    }
                }
            }
            ContentType::Url => {
                if let Some(u) = url {
                    hasher.update(u.as_bytes());
                } else if let Some(t) = text {
                    hasher.update(t.as_bytes());
                }
            }
        }

        // Include secondary plain text when present so mixed pastes differ.
        if !matches!(content_type, ContentType::Text) {
            if let Some(t) = text {
                hasher.update([0xfe]);
                hasher.update(t.as_bytes());
            }
        }

        hex_encode(&hasher.finalize())
    }
}

/// In-memory ring buffer of recent clipboard items (newest at front).
#[derive(Debug)]
pub struct History {
    items: VecDeque<ClipboardItem>,
    max_size: usize,
}

impl History {
    pub fn new(max_size: usize) -> Self {
        Self {
            items: VecDeque::new(),
            max_size: max_size.max(1),
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Newest-first slice for UI rendering.
    #[allow(dead_code)]
    pub fn items(&self) -> &VecDeque<ClipboardItem> {
        &self.items
    }

    /// Replace cache contents (e.g. after loading from SQLite). Newest first.
    pub fn replace_all(&mut self, items: Vec<ClipboardItem>) {
        self.items = items.into();
        while self.items.len() > self.max_size {
            self.items.pop_back();
        }
    }

    /// Push a new item at the front; drop oldest when over capacity.
    /// Pinned items are sorted to the front on next full replace; for live
    /// capture we keep newest-first (UI may re-sort pinned first when rendering).
    pub fn push_front(&mut self, item: ClipboardItem) {
        // Avoid consecutive in-memory duplicates (same hash).
        if let Some(front) = self.items.front() {
            if front.hash == item.hash {
                // Move existing to front with updated metadata.
                let mut existing = self.items.pop_front().expect("front exists");
                existing.created_at = item.created_at.clone();
                existing.id = item.id;
                self.items.push_front(existing);
                return;
            }
        }
        self.items.push_front(item);
        while self.items.len() > self.max_size {
            // Prefer dropping unpinned from the back.
            if let Some(pos) = self
                .items
                .iter()
                .rposition(|i| !i.is_pinned)
            {
                self.items.remove(pos);
            } else {
                self.items.pop_back();
            }
        }
    }

    /// Find item by database id.
    pub fn get(&self, id: u64) -> Option<&ClipboardItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Update pin flag in cache; re-order so pinned items come first.
    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> bool {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return false;
        };
        if let Some(item) = self.items.get_mut(pos) {
            item.is_pinned = pinned;
        }
        self.sort_pinned_first();
        true
    }

    /// Remove item from cache by id.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.id != id);
        self.items.len() != before
    }

    /// Remove many items by id.
    pub fn remove_many(&mut self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        self.items.retain(|i| !ids.contains(&i.id));
    }

    /// Drop unpinned items from the in-memory cache.
    pub fn clear_unpinned(&mut self) {
        self.items.retain(|i| i.is_pinned);
    }

    /// Empty the in-memory cache.
    #[allow(dead_code)]
    pub fn clear_all(&mut self) {
        self.items.clear();
    }

    /// Case-insensitive substring filter over preview / text / url / paths.
    pub fn filter(&self, query: &str) -> Vec<ClipboardItem> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.items.iter().cloned().collect();
        }
        self.items
            .iter()
            .filter(|item| item_matches_query(item, &q))
            .cloned()
            .collect()
    }

    fn sort_pinned_first(&mut self) {
        let mut v: Vec<_> = self.items.drain(..).collect();
        v.sort_by(|a, b| match (a.is_pinned, b.is_pinned) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal, // keep relative order (stable)
        });
        // stable sort by created_at among same pin group is already approximate;
        // use stable sort only on pin flag:
        // Actually Ordering::Equal keeps original order with stable sort.
        self.items = v.into();
    }
}

fn item_matches_query(item: &ClipboardItem, q_lower: &str) -> bool {
    if item.preview.to_lowercase().contains(q_lower) {
        return true;
    }
    if item
        .content_text
        .as_ref()
        .is_some_and(|t| t.to_lowercase().contains(q_lower))
    {
        return true;
    }
    if item
        .content_html
        .as_ref()
        .is_some_and(|t| t.to_lowercase().contains(q_lower))
    {
        return true;
    }
    if item
        .content_url
        .as_ref()
        .is_some_and(|t| t.to_lowercase().contains(q_lower))
    {
        return true;
    }
    if let Some(paths) = &item.content_file_paths {
        if paths.iter().any(|p| p.to_lowercase().contains(q_lower)) {
            return true;
        }
    }
    false
}

/// Write a history item back to the system pasteboard (for copy / re-paste).
pub fn copy_item_to_pasteboard(item: &ClipboardItem) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();

    let mut wrote = false;
    if let Some(ref text) = item.content_text {
        if pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }) {
            wrote = true;
        }
    }
    if let Some(ref html) = item.content_html {
        if pb.setString_forType(&NSString::from_str(html), unsafe { NSPasteboardTypeHTML }) {
            wrote = true;
        }
    }
    if let Some(ref rtf) = item.content_rtf {
        let data = NSData::with_bytes(rtf);
        if pb.setData_forType(Some(&data), unsafe { NSPasteboardTypeRTF }) {
            wrote = true;
        }
    }
    if let Some(ref img) = item.content_image {
        let data = NSData::with_bytes(img);
        // Thumbnails may be JPEG; paste as TIFF if original type unknown.
        if pb.setData_forType(Some(&data), unsafe { NSPasteboardTypePNG })
            || pb.setData_forType(Some(&data), unsafe { NSPasteboardTypeTIFF })
        {
            wrote = true;
        }
    }
    if let Some(ref url) = item.content_url {
        if pb.setString_forType(&NSString::from_str(url), unsafe { NSPasteboardTypeURL }) {
            wrote = true;
        }
        // Also as plain text for apps that only read string.
        if item.content_text.is_none() {
            let _ = pb.setString_forType(&NSString::from_str(url), unsafe {
                NSPasteboardTypeString
            });
            wrote = true;
        }
    }
    if let Some(ref paths) = item.content_file_paths {
        if let Some(first) = paths.first() {
            // Best-effort: file path as plain text (full file pasteboard items are Phase 5+ polish).
            if item.content_text.is_none() {
                let joined = paths.join("\n");
                if pb.setString_forType(&NSString::from_str(&joined), unsafe {
                    NSPasteboardTypeString
                }) {
                    wrote = true;
                }
            }
            let _ = first;
        }
    }

    if wrote {
        info!("copied history item id={} back to pasteboard", item.id);
    } else {
        warn!("copy_item_to_pasteboard: nothing written for id={}", item.id);
    }
}

/// Tracks pasteboard `changeCount` and extracts new captures.
pub struct ClipboardPoller {
    last_change_count: isize,
    /// When true, the next changeCount bump is ignored (self-induced copy-back).
    ignore_next: bool,
}

impl ClipboardPoller {
    pub fn new() -> Self {
        // Seed with current changeCount so we don't re-import the whole board on launch.
        let pb = NSPasteboard::generalPasteboard();
        let count = pb.changeCount() as isize;
        Self {
            last_change_count: count,
            ignore_next: false,
        }
    }

    /// Ignore the next pasteboard change (used after copy-back to avoid dup history).
    pub fn ignore_next_change(&mut self) {
        self.ignore_next = true;
    }

    /// Poll once when the pasteboard `changeCount` advances.
    ///
    /// Privacy check runs **before** any payload extraction so secrets from
    /// password managers never enter memory or SQLite.
    ///
    /// Does **not** touch history or SQLite — the app layer handles persistence.
    pub fn poll(&mut self) -> PollResult {
        let pb = NSPasteboard::generalPasteboard();
        let count = pb.changeCount() as isize;
        if count == self.last_change_count {
            return PollResult::NoChange;
        }
        self.last_change_count = count;

        if self.ignore_next {
            self.ignore_next = false;
            debug!("clipboard changeCount → {count} (ignored self copy-back)");
            return PollResult::NoChange;
        }

        debug!("clipboard changeCount → {count}");

        // Phase 4: inspect types only — do not read string/data for sensitive items.
        let markers = privacy::inspect_pasteboard(&pb);
        if markers.is_sensitive() {
            privacy::log_skipped(markers);
            return PollResult::SkippedPrivate(markers);
        }

        match extract_clipboard_item(&pb) {
            Some(item) => {
                info!(
                    "captured {:?} — {}",
                    item.content_type,
                    truncate_for_log(&item.preview, 80)
                );
                PollResult::Captured(item)
            }
            None => {
                debug!("clipboard change ignored (empty / unsupported types)");
                PollResult::EmptyOrUnsupported
            }
        }
    }
}

impl Default for ClipboardPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Read pasteboard types and build a [`ClipboardItem`] (best-effort multi-type).
fn extract_clipboard_item(pb: &NSPasteboard) -> Option<ClipboardItem> {
    let text = pb
        .stringForType(unsafe { NSPasteboardTypeString })
        .map(|s| s.to_string());
    let html = pb
        .stringForType(unsafe { NSPasteboardTypeHTML })
        .map(|s| s.to_string());
    let rtf = pb
        .dataForType(unsafe { NSPasteboardTypeRTF })
        .map(|d| d.to_vec());
    let url = extract_url(pb);
    let file_paths = extract_file_paths(pb);
    let image = extract_image_thumbnail(pb);

    // Prefer primary type for classification (Text > RTF > HTML > Image > File > URL).
    let content_type = if text.as_ref().is_some_and(|t| !t.trim().is_empty()) {
        ContentType::Text
    } else if rtf.is_some() {
        ContentType::Rtf
    } else if html.as_ref().is_some_and(|h| !h.is_empty()) {
        ContentType::Html
    } else if image.is_some() {
        ContentType::Image
    } else if file_paths.as_ref().is_some_and(|p| !p.is_empty()) {
        ContentType::File
    } else if url.as_ref().is_some_and(|u| !u.is_empty()) {
        ContentType::Url
    } else {
        return None;
    };

    // Drop completely empty captures.
    if text.as_ref().map(|t| t.is_empty()).unwrap_or(true)
        && rtf.is_none()
        && html.as_ref().map(|h| h.is_empty()).unwrap_or(true)
        && image.is_none()
        && file_paths.as_ref().map(|p| p.is_empty()).unwrap_or(true)
        && url.as_ref().map(|u| u.is_empty()).unwrap_or(true)
    {
        return None;
    }

    let hash = ClipboardItem::compute_hash(
        content_type,
        text.as_deref(),
        rtf.as_deref(),
        html.as_deref(),
        image.as_deref(),
        file_paths.as_deref(),
        url.as_deref(),
    );

    let preview = ClipboardItem::make_preview(
        content_type,
        text.as_deref(),
        html.as_deref(),
        url.as_deref(),
        file_paths.as_deref(),
        image.is_some(),
        rtf.is_some(),
    );

    Some(ClipboardItem {
        id: 0, // assigned by SQLite on insert
        content_type,
        content_text: text,
        content_rtf: rtf,
        content_html: html,
        content_image: image,
        content_file_paths: file_paths,
        content_url: url,
        // The app frontmost at capture time is (almost always) the one that copied.
        source_app_bundle_id: frontmost_bundle_id(),
        is_pinned: false,
        created_at: String::new(), // filled by storage on insert
        hash,
        preview,
    })
}

/// Bundle id of the frontmost application, if any.
pub fn frontmost_bundle_id() -> Option<String> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.bundleIdentifier())
        .map(|id| id.to_string())
}

fn extract_url(pb: &NSPasteboard) -> Option<String> {
    if let Some(s) = pb.stringForType(unsafe { NSPasteboardTypeURL }) {
        let s = s.to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Some(s) = pb.stringForType(unsafe { NSPasteboardTypeFileURL }) {
        let s = s.to_string();
        if s.starts_with("http://") || s.starts_with("https://") {
            return Some(s);
        }
    }
    None
}

fn extract_file_paths(pb: &NSPasteboard) -> Option<Vec<String>> {
    #[allow(deprecated)]
    let filenames_type = unsafe { objc2_app_kit::NSFilenamesPboardType };
    if let Some(obj) = pb.propertyListForType(filenames_type) {
        if let Some(paths) = nsarray_of_strings(&obj) {
            if !paths.is_empty() {
                return Some(paths);
            }
        }
    }

    if let Some(s) = pb.stringForType(unsafe { NSPasteboardTypeFileURL }) {
        let s = s.to_string();
        if !s.is_empty() {
            let path = s.strip_prefix("file://").unwrap_or(&s).to_string();
            return Some(vec![path]);
        }
    }

    None
}

fn nsarray_of_strings(obj: &AnyObject) -> Option<Vec<String>> {
    let arr = obj.downcast_ref::<NSArray>()?;
    let mut out = Vec::with_capacity(arr.count() as usize);
    for i in 0..arr.count() {
        let item = arr.objectAtIndex(i);
        if let Some(s) = item.downcast_ref::<NSString>() {
            out.push(s.to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_image_thumbnail(pb: &NSPasteboard) -> Option<Vec<u8>> {
    let data = pb
        .dataForType(unsafe { NSPasteboardTypePNG })
        .or_else(|| pb.dataForType(unsafe { NSPasteboardTypeTIFF }))?;

    make_thumbnail(&data).or_else(|| {
        warn!("thumbnail generation failed; storing capped raw image bytes");
        let raw = data.to_vec();
        if raw.len() > 256 * 1024 {
            Some(raw[..256 * 1024].to_vec())
        } else {
            Some(raw)
        }
    })
}

/// Resize to max 256×256 and encode as JPEG when possible, else TIFF.
fn make_thumbnail(data: &NSData) -> Option<Vec<u8>> {
    let image = NSImage::initWithData(NSImage::alloc(), data)?;
    let size = image.size();
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }

    let scale = (THUMB_MAX / size.width)
        .min(THUMB_MAX / size.height)
        .min(1.0);
    let tw = (size.width * scale).max(1.0);
    let th = (size.height * scale).max(1.0);
    let thumb_size = NSSize::new(tw, th);

    let thumb = NSImage::initWithSize(NSImage::alloc(), thumb_size);
    #[allow(deprecated)]
    {
        thumb.lockFocus();
        image.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), thumb_size));
        thumb.unlockFocus();
    }

    if let Some(tiff) = thumb.TIFFRepresentation() {
        if let Some(rep) = NSBitmapImageRep::imageRepWithData(&tiff) {
            let props = objc2_foundation::NSDictionary::<NSString, AnyObject>::new();
            if let Some(jpeg) = unsafe {
                rep.representationUsingType_properties(NSBitmapImageFileType::JPEG, &props)
            } {
                return Some(jpeg.to_vec());
            }
        }
        return Some(tiff.to_vec());
    }
    None
}

fn file_preview(paths: &[String]) -> String {
    if paths.is_empty() {
        return "[Files]".to_string();
    }
    if paths.len() == 1 {
        return truncate_preview(&paths[0]);
    }
    truncate_preview(&format!("{} files: {}", paths.len(), paths[0]))
}

fn truncate_preview(s: &str) -> String {
    const MAX: usize = 120;
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= MAX {
        flat.to_string()
    } else {
        let truncated: String = flat.chars().take(MAX.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[allow(dead_code)]
fn _now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _retain_marker(_: &Retained<NSString>) {}
