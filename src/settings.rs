//! User preferences persisted in the SQLite `settings` table.
//!
//! Phase 7: poll interval, retention, hotkey preset, auto-paste, launch at login.

use log::{info, warn};

use crate::storage::Storage;

const KEY_AUTO_PASTE: &str = "auto_paste";
const KEY_POLL_MS: &str = "poll_interval_ms";
const KEY_RETENTION_DAYS: &str = "retention_days";
const KEY_HISTORY_LIMIT: &str = "history_limit";
const KEY_HOTKEY: &str = "hotkey_preset";
const KEY_LAUNCH_AT_LOGIN: &str = "launch_at_login";
const KEY_FLOAT_PANEL: &str = "float_panel";

/// Preset global hotkey combinations (v1 — dropdown, not free-form recorder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HotkeyPreset {
    #[default]
    CtrlCmdV,
    CmdShiftV,
    CmdShiftC,
    CmdOptionV,
    CtrlShiftV,
}

impl HotkeyPreset {
    pub const ALL: [HotkeyPreset; 5] = [
        Self::CtrlCmdV,
        Self::CmdShiftV,
        Self::CmdShiftC,
        Self::CmdOptionV,
        Self::CtrlShiftV,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CtrlCmdV => "ctrl+cmd+v",
            Self::CmdShiftV => "cmd+shift+v",
            Self::CmdShiftC => "cmd+shift+c",
            Self::CmdOptionV => "cmd+option+v",
            Self::CtrlShiftV => "ctrl+shift+v",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Self::CtrlCmdV => "⌃⌘V",
            Self::CmdShiftV => "⌘⇧V",
            Self::CmdShiftC => "⌘⇧C",
            Self::CmdOptionV => "⌘⌥V",
            Self::CtrlShiftV => "⌃⇧V",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "cmd+shift+v" | "command+shift+v" => Self::CmdShiftV,
            "cmd+shift+c" | "command+shift+c" => Self::CmdShiftC,
            "cmd+option+v" | "cmd+alt+v" | "command+option+v" => Self::CmdOptionV,
            "ctrl+shift+v" | "control+shift+v" => Self::CtrlShiftV,
            _ => Self::CtrlCmdV,
        }
    }

    /// Index in [`HotkeyPreset::ALL`] / popup menu.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    /// Carbon virtual key code for the letter key.
    pub fn key_code(self) -> u16 {
        match self {
            Self::CtrlCmdV | Self::CmdShiftV | Self::CmdOptionV | Self::CtrlShiftV => 0x09, // V
            Self::CmdShiftC => 0x08,                                       // C
        }
    }
}

/// Application settings with sensible defaults for v1.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Clipboard poll interval in milliseconds (min 250).
    pub poll_interval_ms: u64,
    /// Time-based retention for unpinned items (0 = unlimited / skip time prune).
    pub retention_days: i64,
    /// Max unpinned history items to retain (count-based pruning).
    pub history_limit: usize,
    /// Whether to auto-paste (⌘V) after selecting an item.
    pub auto_paste: bool,
    /// Whether to launch ZnonClip at login (SMAppService).
    pub launch_at_login: bool,
    /// Global hotkey preset.
    pub hotkey: HotkeyPreset,
    /// Keep the floater above other windows and open after a paste.
    /// Off: it behaves like a menu and closes on paste or an outside click.
    pub float_panel: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_interval_ms: 500,
            retention_days: 30,
            history_limit: DEFAULT_HISTORY_ITEMS,
            // Enter in the popover should paste, like ⌘V; without Accessibility
            // permission this degrades to copy-only with a status notice.
            auto_paste: true,
            launch_at_login: false,
            hotkey: HotkeyPreset::default(),
            float_panel: true,
        }
    }
}

/// Default number of unpinned (recent) items kept.
pub const DEFAULT_HISTORY_ITEMS: usize = 20;

/// Hard cap on pinned items. Pinning beyond this is refused with a notice.
pub const MAX_PINNED: usize = 20;

/// Poll interval options shown in the UI (ms).
pub const POLL_INTERVAL_OPTIONS: [u64; 4] = [250, 500, 1000, 2000];

/// Retention day options (`0` = unlimited time-based prune).
pub const RETENTION_DAY_OPTIONS: [i64; 5] = [7, 30, 90, 365, 0];

/// History max-count options.
pub const HISTORY_LIMIT_OPTIONS: [usize; 5] = [20, 50, 100, 500, 1000];

impl Settings {
    /// Load settings from SQLite (missing keys keep defaults).
    pub fn load(storage: &Storage) -> Self {
        let mut s = Self::default();

        if let Ok(Some(v)) = storage.get_setting(KEY_AUTO_PASTE) {
            s.auto_paste = parse_bool(&v);
        }
        if let Ok(Some(v)) = storage.get_setting(KEY_POLL_MS) {
            if let Ok(n) = v.parse::<u64>() {
                s.poll_interval_ms = n.max(250);
            }
        }
        if let Ok(Some(v)) = storage.get_setting(KEY_RETENTION_DAYS) {
            if let Ok(n) = v.parse::<i64>() {
                s.retention_days = n.max(0);
            }
        }
        if let Ok(Some(v)) = storage.get_setting(KEY_HISTORY_LIMIT) {
            if let Ok(n) = v.parse::<usize>() {
                s.history_limit = n.max(5);
            }
        }
        if let Ok(Some(v)) = storage.get_setting(KEY_HOTKEY) {
            s.hotkey = HotkeyPreset::from_str(&v);
        }
        if let Ok(Some(v)) = storage.get_setting(KEY_LAUNCH_AT_LOGIN) {
            s.launch_at_login = parse_bool(&v);
        }
        if let Ok(Some(v)) = storage.get_setting(KEY_FLOAT_PANEL) {
            s.float_panel = parse_bool(&v);
        }

        info!(
            "settings loaded (poll={}ms, retention={}d, limit={}, auto_paste={}, hotkey={}, login={})",
            s.poll_interval_ms,
            s.retention_days,
            s.history_limit,
            s.auto_paste,
            s.hotkey.as_str(),
            s.launch_at_login
        );
        s
    }

    /// Persist all settings keys.
    pub fn save_all(&self, storage: &Storage) {
        self.save_auto_paste(storage);
        self.save_key(storage, KEY_POLL_MS, &self.poll_interval_ms.to_string());
        self.save_key(storage, KEY_RETENTION_DAYS, &self.retention_days.to_string());
        self.save_key(storage, KEY_HISTORY_LIMIT, &self.history_limit.to_string());
        self.save_key(storage, KEY_HOTKEY, self.hotkey.as_str());
        self.save_key(
            storage,
            KEY_LAUNCH_AT_LOGIN,
            if self.launch_at_login { "1" } else { "0" },
        );
        self.save_key(storage, KEY_FLOAT_PANEL, if self.float_panel { "1" } else { "0" });
    }

    /// Persist auto-paste flag.
    pub fn save_auto_paste(&self, storage: &Storage) {
        self.save_key(
            storage,
            KEY_AUTO_PASTE,
            if self.auto_paste { "1" } else { "0" },
        );
    }

    fn save_key(&self, storage: &Storage, key: &str, value: &str) {
        if let Err(e) = storage.set_setting(key, value) {
            warn!("failed to save setting {key}: {e}");
        }
    }

    /// Index into [`POLL_INTERVAL_OPTIONS`] (nearest).
    pub fn poll_interval_index(&self) -> usize {
        nearest_index(
            &POLL_INTERVAL_OPTIONS.map(|v| v as i64),
            self.poll_interval_ms as i64,
        )
    }

    pub fn retention_index(&self) -> usize {
        nearest_index(&RETENTION_DAY_OPTIONS, self.retention_days)
    }

    pub fn history_limit_index(&self) -> usize {
        nearest_index(
            &HISTORY_LIMIT_OPTIONS.map(|v| v as i64),
            self.history_limit as i64,
        )
    }
}

fn parse_bool(v: &str) -> bool {
    v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
}

fn nearest_index(options: &[i64], value: i64) -> usize {
    options
        .iter()
        .enumerate()
        .min_by_key(|(_, &opt)| (opt - value).unsigned_abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Label for retention popup (days).
pub fn retention_label(days: i64) -> String {
    if days == 0 {
        "Unlimited (time)".to_string()
    } else {
        format!("{days} days")
    }
}

/// Label for poll interval popup.
pub fn poll_label(ms: u64) -> String {
    if ms >= 1000 {
        format!("{} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

/// Label for history limit popup.
pub fn history_limit_label(n: usize) -> String {
    format!("{n} items")
}
