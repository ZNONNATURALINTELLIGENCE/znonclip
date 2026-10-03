//! Dev helper: write a password-manager-style concealed pasteboard item.
//!
//! Usage:
//!   cargo run --example write_concealed -- "my-secret"
//!   cargo run --example write_concealed -- --transient "temp-secret"
//!
//! Used to verify Phase 4 privacy filtering without installing 1Password.

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSData, NSString};

const CONCEALED: &str = "org.nspasteboard.ConcealedType";
const TRANSIENT: &str = "org.nspasteboard.TransientType";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut transient = false;
    if args.first().map(|s| s.as_str()) == Some("--transient") {
        transient = true;
        args.remove(0);
    }
    let secret = args
        .first()
        .cloned()
        .unwrap_or_else(|| "clip-assistant-test-secret".to_string());

    let pb = NSPasteboard::generalPasteboard();
    // One clear + declare both types, then set marker and string immediately.
    // Real password managers typically publish both types in a single pasteboard update.
    pb.clearContents();

    let plain = unsafe { NSPasteboardTypeString };
    let marker = NSString::from_str(if transient { TRANSIENT } else { CONCEALED });
    let types = NSArray::from_slice(&[plain, &*marker]);
    // SAFETY: valid pasteboard type strings; no owner.
    let _ = unsafe { pb.declareTypes_owner(&types, None) };

    // Marker first so concurrent pollers never see bare plaintext.
    assert!(
        pb.setData_forType(Some(&NSData::with_bytes(&[])), &marker),
        "failed to set privacy marker"
    );
    assert!(
        pb.setString_forType(&NSString::from_str(&secret), plain),
        "failed to set secret string"
    );

    let kinds = if transient { "TransientType" } else { "ConcealedType" };
    let type_names: Vec<String> = pb
        .types()
        .map(|arr| {
            (0..arr.count())
                .map(|i| arr.objectAtIndex(i).to_string())
                .collect()
        })
        .unwrap_or_default();
    eprintln!(
        "wrote pasteboard with {kinds} (content length {}, types={type_names:?})",
        secret.len()
    );
}
