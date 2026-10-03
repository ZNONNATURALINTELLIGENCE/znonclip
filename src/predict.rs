//! Predictive paste: a local, advisory ranker over recent + pinned items.
//!
//! When the popover opens, the ranker scores every visible item and the UI
//! **pre-highlights** the top one. That is all it does:
//!
//! * Enter pastes the highlighted row; arrow keys move the highlight freely.
//! * List order never changes, so muscle memory stays stable.
//! * Typing a search query switches prediction off (the user said what they want).
//! * If ranking fails or no ranker is configured, the highlight falls back to
//!   the first row: the same behaviour as a plain clipboard manager.
//!
//! The deterministic paste path owns the action; the ranker only proposes.
//!
//! ## Slotting in a model
//!
//! Every ranker consumes the same [`Features`] vector built by
//! [`Features::extract`]. The built-in [`HeuristicRanker`] is a hand-weighted
//! linear scorer over it. A learned local model (for example a small on-device
//! decision model) implements [`Ranker`] over the same features, so swapping it
//! in needs no change to capture, storage or UI. See `docs/architecture.md`.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::clipboard::ClipboardItem;
use crate::status_item::parse_iso8601_to_unix_secs;
use crate::storage::PasteStats;

/// What the ranker knows about the moment of paste. Built once per popover open.
#[derive(Debug, Default, Clone)]
pub struct PasteContext {
    /// Bundle id of the app that will receive the paste (frontmost before open).
    pub target_app: Option<String>,
    /// Hash of whatever is on the system pasteboard right now, if known.
    pub current_clipboard_hash: Option<String>,
    /// Past paste events into `target_app`.
    pub stats: PasteStats,
    /// Wall clock, Unix seconds.
    pub now_unix: i64,
}

impl PasteContext {
    pub fn new(target_app: Option<String>, current_clipboard_hash: Option<String>, stats: PasteStats) -> Self {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            target_app,
            current_clipboard_hash,
            stats,
            now_unix,
        }
    }
}

/// Per-item feature vector, all values in `[0, 1]`. Stable public contract
/// between capture/storage and any ranker implementation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Features {
    /// 1.0 for the newest unpinned item, decaying by half per position. 0 for pins.
    pub recency_rank: f32,
    /// Age decay: 1.0 just copied, ~0.5 after 30 minutes. 0 for pins.
    pub freshness: f32,
    /// 1.0 when pinned.
    pub pinned: f32,
    /// Share of past pastes into the target app that were this exact item.
    pub item_affinity: f32,
    /// Share of past pastes into the target app that had this content type.
    pub type_affinity: f32,
    /// 1.0 when the item was copied from the app it is about to be pasted into.
    pub same_app: f32,
    /// 1.0 when the item is already on the system pasteboard (plain ⌘V would do).
    pub on_clipboard: f32,
}

/// Half-life for [`Features::freshness`], in seconds.
const FRESHNESS_HALF_LIFE_SECS: f32 = 30.0 * 60.0;

impl Features {
    /// Build features for `item`. `unpinned_rank` is its 0-based position among
    /// unpinned items, newest first (ignored for pins).
    pub fn extract(item: &ClipboardItem, unpinned_rank: usize, ctx: &PasteContext) -> Self {
        let pinned = item.is_pinned;

        let (recency_rank, freshness) = if pinned {
            (0.0, 0.0)
        } else {
            let rank = 0.5_f32.powi(unpinned_rank.min(30) as i32);
            let fresh = parse_iso8601_to_unix_secs(&item.created_at)
                .map(|t| {
                    let age = (ctx.now_unix - t).max(0) as f32;
                    0.5_f32.powf(age / FRESHNESS_HALF_LIFE_SECS)
                })
                .unwrap_or(0.0);
            (rank, fresh)
        };

        let total = ctx.stats.total.max(1) as f32;
        let item_affinity = ctx.stats.per_item.get(&item.id).copied().unwrap_or(0) as f32 / total;
        let type_affinity = ctx
            .stats
            .per_type
            .get(&item.content_type)
            .copied()
            .unwrap_or(0) as f32
            / total;

        let same_app = match (&ctx.target_app, &item.source_app_bundle_id) {
            (Some(t), Some(s)) if t == s => 1.0,
            _ => 0.0,
        };
        let on_clipboard = match &ctx.current_clipboard_hash {
            Some(h) if *h == item.hash => 1.0,
            _ => 0.0,
        };

        Self {
            recency_rank,
            freshness,
            pinned: if pinned { 1.0 } else { 0.0 },
            item_affinity,
            type_affinity,
            same_app,
            on_clipboard,
        }
    }
}

/// A ranker scores items; higher means "more likely the one to paste".
pub trait Ranker {
    /// Short identifier for logs.
    fn name(&self) -> &'static str;
    /// Score one feature vector.
    fn score(&self, f: &Features) -> f32;
    /// One-line, human-readable reason for the top pick (shown as a tooltip).
    fn explain(&self, f: &Features) -> &'static str;
}

/// Hand-weighted linear scorer. Weights are deliberately simple and tested.
#[derive(Debug, Clone, Copy)]
pub struct HeuristicRanker {
    pub w_recency_rank: f32,
    pub w_freshness: f32,
    pub w_pinned: f32,
    pub w_item_affinity: f32,
    pub w_type_affinity: f32,
    pub w_same_app: f32,
    /// Subtracted: you opened the history, so plain ⌘V was probably not enough.
    pub w_on_clipboard: f32,
}

impl Default for HeuristicRanker {
    fn default() -> Self {
        Self {
            w_recency_rank: 1.0,
            w_freshness: 0.3,
            w_pinned: 0.25,
            w_item_affinity: 1.5,
            w_type_affinity: 0.3,
            w_same_app: 0.15,
            w_on_clipboard: 0.6,
        }
    }
}

impl Ranker for HeuristicRanker {
    fn name(&self) -> &'static str {
        "heuristic-v1"
    }

    fn score(&self, f: &Features) -> f32 {
        self.w_recency_rank * f.recency_rank
            + self.w_freshness * f.freshness
            + self.w_pinned * f.pinned
            + self.w_item_affinity * f.item_affinity
            + self.w_type_affinity * f.type_affinity
            + self.w_same_app * f.same_app
            - self.w_on_clipboard * f.on_clipboard
    }

    fn explain(&self, f: &Features) -> &'static str {
        let parts = [
            (self.w_item_affinity * f.item_affinity, "often pasted into this app"),
            (self.w_recency_rank * f.recency_rank + self.w_freshness * f.freshness, "most recent copy"),
            (self.w_pinned * f.pinned, "pinned"),
            (self.w_type_affinity * f.type_affinity, "this app usually gets this kind of content"),
            (self.w_same_app * f.same_app, "copied from this app"),
        ];
        parts
            .iter()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|p| p.1)
            .unwrap_or("most recent copy")
    }
}

/// The ranker's proposal for one popover open.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub item_id: u64,
    pub score: f32,
    pub reason: &'static str,
}

/// Rank `items` (any order) and return the top pick, or `None` when empty.
/// Ties go to the earlier item, so the result is deterministic.
pub fn predict(ranker: &dyn Ranker, items: &[ClipboardItem], ctx: &PasteContext) -> Option<Prediction> {
    // Unpinned rank by created_at, newest first (input order is not trusted).
    let mut unpinned: Vec<(i64, usize)> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| !i.is_pinned)
        .map(|(idx, i)| (parse_iso8601_to_unix_secs(&i.created_at).unwrap_or(0), idx))
        .collect();
    unpinned.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut rank_of = vec![0usize; items.len()];
    for (rank, (_, idx)) in unpinned.iter().enumerate() {
        rank_of[*idx] = rank;
    }

    let mut best: Option<(f32, usize, Features)> = None;
    for (idx, item) in items.iter().enumerate() {
        let f = Features::extract(item, rank_of[idx], ctx);
        let s = ranker.score(&f);
        if !s.is_finite() {
            continue;
        }
        if best.as_ref().map_or(true, |(bs, _, _)| s > *bs) {
            best = Some((s, idx, f));
        }
    }
    best.map(|(score, idx, f)| Prediction {
        item_id: items[idx].id,
        score,
        reason: ranker.explain(&f),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ContentType;

    const NOW: i64 = 1_790_000_000;

    fn iso(secs_ago: i64) -> String {
        // Reuse storage's formatter via a round trip through the parser's format.
        let t = NOW - secs_ago;
        let days = t.div_euclid(86_400);
        let rem = t.rem_euclid(86_400);
        // days → civil (same algorithm as storage.rs)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    fn item(id: u64, secs_ago: i64, pinned: bool, ct: ContentType, src: Option<&str>) -> ClipboardItem {
        ClipboardItem {
            id,
            content_type: ct,
            content_text: Some(format!("item {id}")),
            content_rtf: None,
            content_html: None,
            content_image: None,
            content_file_paths: None,
            content_url: None,
            source_app_bundle_id: src.map(str::to_string),
            is_pinned: pinned,
            created_at: iso(secs_ago),
            hash: format!("h{id}"),
            preview: format!("item {id}"),
        }
    }

    fn ctx(target: Option<&str>, clip: Option<&str>, stats: PasteStats) -> PasteContext {
        PasteContext {
            target_app: target.map(str::to_string),
            current_clipboard_hash: clip.map(str::to_string),
            stats,
            now_unix: NOW,
        }
    }

    #[test]
    fn iso_helper_round_trips() {
        assert_eq!(parse_iso8601_to_unix_secs(&iso(0)), Some(NOW));
        assert_eq!(parse_iso8601_to_unix_secs(&iso(3_661)), Some(NOW - 3_661));
    }

    #[test]
    fn empty_list_predicts_nothing() {
        let r = HeuristicRanker::default();
        assert_eq!(predict(&r, &[], &ctx(None, None, PasteStats::default())), None);
    }

    #[test]
    fn cold_start_picks_newest_copy() {
        let r = HeuristicRanker::default();
        let items = vec![
            item(1, 600, false, ContentType::Text, None),
            item(2, 10, false, ContentType::Text, None),
            item(3, 300, true, ContentType::Text, None),
        ];
        let p = predict(&r, &items, &ctx(None, None, PasteStats::default())).unwrap();
        assert_eq!(p.item_id, 2);
        assert_eq!(p.reason, "most recent copy");
    }

    #[test]
    fn item_already_on_clipboard_yields_to_the_next_newest() {
        let r = HeuristicRanker::default();
        let items = vec![
            item(1, 5, false, ContentType::Text, None),
            item(2, 60, false, ContentType::Text, None),
        ];
        let p = predict(&r, &items, &ctx(None, Some("h1"), PasteStats::default())).unwrap();
        assert_eq!(p.item_id, 2);
    }

    #[test]
    fn app_paste_history_beats_recency() {
        let r = HeuristicRanker::default();
        let items = vec![
            item(1, 5, false, ContentType::Text, None),
            item(2, 900, false, ContentType::Text, None),
            item(7, 86_400, true, ContentType::Text, None),
        ];
        let mut stats = PasteStats::default();
        stats.per_item.insert(7, 8);
        stats.per_item.insert(1, 1);
        stats.per_type.insert(ContentType::Text, 9);
        stats.total = 9;
        let p = predict(&r, &items, &ctx(Some("com.example.term"), None, stats)).unwrap();
        assert_eq!(p.item_id, 7);
        assert_eq!(p.reason, "often pasted into this app");
    }

    #[test]
    fn type_affinity_breaks_near_ties() {
        let r = HeuristicRanker::default();
        // Same age; the URL wins in an app that historically receives URLs.
        let items = vec![
            item(1, 100, false, ContentType::Text, None),
            item(2, 100, false, ContentType::Url, None),
        ];
        let mut stats = PasteStats::default();
        stats.per_type.insert(ContentType::Url, 5);
        stats.total = 5;
        // Equal timestamps: item 1 gets rank 0 by stable tie-break, so the URL must
        // overcome a recency gap of 0.5; type affinity alone (0.3) does not.
        let p = predict(&r, &items, &ctx(Some("com.example.browser"), None, stats.clone())).unwrap();
        assert_eq!(p.item_id, 1, "type affinity alone must not override a full recency step");
        // Add item-level paste history for the URL and it wins.
        stats.per_item.insert(2, 2);
        let p = predict(&r, &items, &ctx(Some("com.example.browser"), None, stats)).unwrap();
        assert_eq!(p.item_id, 2);
    }

    #[test]
    fn non_finite_scores_are_skipped() {
        struct Nan;
        impl Ranker for Nan {
            fn name(&self) -> &'static str {
                "nan"
            }
            fn score(&self, _: &Features) -> f32 {
                f32::NAN
            }
            fn explain(&self, _: &Features) -> &'static str {
                ""
            }
        }
        let items = vec![item(1, 5, false, ContentType::Text, None)];
        assert_eq!(predict(&Nan, &items, &ctx(None, None, PasteStats::default())), None);
    }

    #[test]
    fn features_are_bounded() {
        let mut stats = PasteStats::default();
        stats.per_item.insert(1, 3);
        stats.per_type.insert(ContentType::Text, 3);
        stats.total = 3;
        let c = ctx(Some("a"), Some("h1"), stats);
        let f = Features::extract(&item(1, 0, false, ContentType::Text, Some("a")), 0, &c);
        for v in [f.recency_rank, f.freshness, f.pinned, f.item_affinity, f.type_affinity, f.same_app, f.on_clipboard] {
            assert!((0.0..=1.0).contains(&v), "{v}");
        }
        assert_eq!(f.same_app, 1.0);
        assert_eq!(f.on_clipboard, 1.0);
    }
}
