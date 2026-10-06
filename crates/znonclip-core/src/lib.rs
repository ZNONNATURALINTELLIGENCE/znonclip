//! ZnonClip Core: the CLI's clip store and the secret scrubber.
//!
//! Used by `znonclip-cli` and `znonclip-agent`. The macOS app keeps its own
//! history database (`com.znonclip.app/znonclip.db`); this crate's store is a
//! separate file (`com.znonclip.ZnonClip/clips.db`).

pub mod scrub;
pub mod storage;

pub use scrub::scrub_secrets;
pub use storage::ClipStore;
