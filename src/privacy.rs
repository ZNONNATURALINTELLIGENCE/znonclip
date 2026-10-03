//! Sensitive clipboard filtering (`ConcealedType` / `TransientType`).
//!
//! Password managers (1Password, Bitwarden, etc.) mark secret clipboard
//! payloads with convention UTIs from [nspasteboard.org](https://nspasteboard.org/):
//!
//! - `org.nspasteboard.ConcealedType` — content must not be stored in history
//! - `org.nspasteboard.TransientType` — temporary content; do not persist
//!
//! Clip Assistant checks pasteboard **types only** before reading payload data, so
//! secret bytes are never pulled into memory or written to SQLite.

use log::info;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

/// Marker set by password managers: do not keep this pasteboard content.
pub const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

/// Marker for short-lived pasteboard content that should not be archived.
pub const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// Which privacy markers were found on a pasteboard change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrivacyMarkers {
    pub concealed: bool,
    pub transient: bool,
}

impl PrivacyMarkers {
    /// True if either marker is present (item must be skipped entirely).
    pub fn is_sensitive(self) -> bool {
        self.concealed || self.transient
    }

    /// Human-readable marker list for logs/UI (never includes secret content).
    pub fn describe(self) -> String {
        match (self.concealed, self.transient) {
            (true, true) => "ConcealedType + TransientType".to_string(),
            (true, false) => "ConcealedType".to_string(),
            (false, true) => "TransientType".to_string(),
            (false, false) => "none".to_string(),
        }
    }
}

/// Inspect a list of pasteboard type UTI strings for privacy markers.
pub fn inspect_types(types: &[String]) -> PrivacyMarkers {
    let mut markers = PrivacyMarkers::default();
    for t in types {
        // Case-sensitive UTI match (canonical forms are as defined by nspasteboard.org).
        if t == CONCEALED_TYPE {
            markers.concealed = true;
        } else if t == TRANSIENT_TYPE {
            markers.transient = true;
        }
    }
    markers
}

/// Returns `true` if the type list indicates sensitive content.
#[allow(dead_code)] // Public helper for tests and future callers
pub fn is_sensitive(types: &[String]) -> bool {
    inspect_types(types).is_sensitive()
}

/// Read available types from `NSPasteboard` without reading payload **contents**.
///
/// Only type UTIs are collected — never `stringForType` / `dataForType` for
/// the secret payload itself.
pub fn pasteboard_type_strings(pb: &NSPasteboard) -> Vec<String> {
    let Some(types) = pb.types() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(types.count() as usize);
    for i in 0..types.count() {
        let t = types.objectAtIndex(i);
        // NSPasteboardType is NSString
        out.push(t.to_string());
    }
    out
}

/// Inspect the pasteboard for privacy markers (types only — no secret payload).
///
/// Uses both `types()` and `availableTypeFromArray:` so markers are found even
/// when declared with empty data (password-manager style).
pub fn inspect_pasteboard(pb: &NSPasteboard) -> PrivacyMarkers {
    let mut markers = inspect_types(&pasteboard_type_strings(pb));

    // Explicit availability check (works when marker type is declared with empty bytes).
    let concealed = NSString::from_str(CONCEALED_TYPE);
    let transient = NSString::from_str(TRANSIENT_TYPE);
    let probe = objc2_foundation::NSArray::from_slice(&[&*concealed, &*transient]);
    if let Some(found) = pb.availableTypeFromArray(&probe) {
        let name = found.to_string();
        if name == CONCEALED_TYPE {
            markers.concealed = true;
        } else if name == TRANSIENT_TYPE {
            markers.transient = true;
        }
    }
    // availableTypeFromArray returns only the first match — probe each marker alone.
    let only_concealed = objc2_foundation::NSArray::from_slice(&[&*concealed]);
    if pb.availableTypeFromArray(&only_concealed).is_some() {
        markers.concealed = true;
    }
    let only_transient = objc2_foundation::NSArray::from_slice(&[&*transient]);
    if pb.availableTypeFromArray(&only_transient).is_some() {
        markers.transient = true;
    }

    markers
}

/// Log that a clipboard change was skipped for privacy — **never** log content.
pub fn log_skipped(markers: PrivacyMarkers) {
    // Use info so it's visible with default RUST_LOG=info during manual testing.
    info!(
        "privacy: skipped clipboard change ({}) — content not stored",
        markers.describe()
    );
}

/// Short status-line text for the popover (no secret data).
pub fn status_message(markers: PrivacyMarkers) -> String {
    format!("Last copy skipped (privacy: {})", markers.describe())
}

/// Helper used by tests / future tooling: does this NSString type match a marker?
#[allow(dead_code)]
pub fn nsstring_is_marker(s: &NSString) -> bool {
    let t = s.to_string();
    t == CONCEALED_TYPE || t == TRANSIENT_TYPE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_concealed() {
        let types = vec![
            "public.utf8-plain-text".into(),
            CONCEALED_TYPE.into(),
        ];
        let m = inspect_types(&types);
        assert!(m.concealed);
        assert!(!m.transient);
        assert!(m.is_sensitive());
        assert!(is_sensitive(&types));
    }

    #[test]
    fn detects_transient() {
        let types = vec![TRANSIENT_TYPE.into(), "public.utf8-plain-text".into()];
        let m = inspect_types(&types);
        assert!(!m.concealed);
        assert!(m.transient);
        assert!(m.is_sensitive());
    }

    #[test]
    fn detects_both() {
        let types = vec![
            CONCEALED_TYPE.into(),
            TRANSIENT_TYPE.into(),
            "public.utf8-plain-text".into(),
        ];
        let m = inspect_types(&types);
        assert!(m.concealed && m.transient && m.is_sensitive());
        assert_eq!(m.describe(), "ConcealedType + TransientType");
    }

    #[test]
    fn normal_types_not_sensitive() {
        let types = vec![
            "public.utf8-plain-text".into(),
            "public.html".into(),
            "public.tiff".into(),
        ];
        assert!(!is_sensitive(&types));
        assert!(!inspect_types(&types).is_sensitive());
    }

    #[test]
    fn empty_types_not_sensitive() {
        assert!(!is_sensitive(&[]));
    }

    /// Writes a concealed payload to the real pasteboard and verifies detection.
    ///
    /// Ignored by default (mutates system clipboard). Run with:
    /// `cargo test privacy::tests::live_pasteboard_concealed -- --ignored --nocapture`
    #[test]
    #[ignore = "mutates system clipboard"]
    fn live_pasteboard_concealed() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::{NSArray, NSData, NSString};

        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();

        let plain = unsafe { NSPasteboardTypeString };
        let concealed = NSString::from_str(CONCEALED_TYPE);
        // NSPasteboardType is NSString — declare both plain text and the privacy marker.
        let types = NSArray::from_slice(&[plain, &*concealed]);
        // SAFETY: type array is valid NSPasteboardType strings; owner is None.
        let _ = unsafe { pb.declareTypes_owner(&types, None) };

        // Set the privacy marker *before* the secret so a concurrent poller never
        // sees plaintext without ConcealedType (password managers write atomically).
        let empty = NSData::with_bytes(&[]);
        assert!(pb.setData_forType(Some(&empty), &concealed));
        assert!(pb.setString_forType(&NSString::from_str("super-secret-password"), plain));

        let markers = inspect_pasteboard(&pb);
        assert!(
            markers.concealed,
            "expected ConcealedType on pasteboard, types={:?}",
            pasteboard_type_strings(&pb)
        );
        assert!(markers.is_sensitive());
        // Critical: we never assert on the secret string here — only markers.
    }

    /// Same as above for TransientType (Bitwarden-style).
    #[test]
    #[ignore = "mutates system clipboard"]
    fn live_pasteboard_transient() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::{NSArray, NSData, NSString};

        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();

        let plain = unsafe { NSPasteboardTypeString };
        let transient = NSString::from_str(TRANSIENT_TYPE);
        let types = NSArray::from_slice(&[plain, &*transient]);
        let _ = unsafe { pb.declareTypes_owner(&types, None) };
        // Marker first, then payload (same ordering rationale as ConcealedType test).
        assert!(pb.setData_forType(Some(&NSData::with_bytes(&[])), &transient));
        assert!(pb.setString_forType(&NSString::from_str("temp-secret"), plain));

        let markers = inspect_pasteboard(&pb);
        assert!(markers.transient && markers.is_sensitive());
    }
}
