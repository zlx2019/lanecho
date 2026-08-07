//! Application settings, persisted as settings.json in the data directory.
//!
//! Only the listen port needs a restart to take effect (the socket is fixed at
//! startup); everything else applies immediately. The display name is not
//! here: identity.json is its single source of truth, hot-updated through the
//! engine by the set_display_name command, which keeps two persistence layers
//! from drifting apart.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Settings file name
const SETTINGS_FILE: &str = "settings.json";

/// User settings
///
/// `rename_all = camelCase` lines up with the frontend DTO; `default` fills in
/// fields that older JSON lacks, which is what makes adding a field backward
/// compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// TCP listen port (0 means random; takes effect after a restart)
    pub tcp_port: u16,
    /// Launch at login (written to the system as soon as settings are saved)
    pub autostart: bool,
    /// Coarse sync toggle, a redundant field always equal to
    /// `sync_mode != "off"`: double-written on save so older versions and
    /// downgrades still read the right on/off semantics
    pub sync_enabled: bool,
    /// Sync direction policy: "off" / "both" / "send" / "receive"
    pub sync_mode: String,
    /// Sync type toggle: text (one toggle with bidirectional meaning, gating
    /// both sending and receiving)
    pub sync_text: bool,
    /// Sync type toggle: images
    pub sync_images: bool,
    /// Sync type toggle: files
    pub sync_files: bool,
    /// Total file sync cap in MB, 1~512; images are fixed at 16MB and are not
    /// covered here
    pub max_sync_file_mb: u32,
    /// Post a system notification when a remote overwrites the clipboard
    /// ("Synced from X"; takes effect immediately)
    pub notify_on_sync: bool,
    /// UI language: "zh" / "en"; empty means uninitialized (the frontend
    /// writes it on first launch after detecting the system language)
    pub language: String,
    /// Cap on retained history entries (over the cap, the oldest unpinned
    /// entry is evicted)
    pub history_max_entries: usize,
    /// History record type toggle: text
    pub history_record_text: bool,
    /// History record type toggle: images
    pub history_record_images: bool,
    /// History record type toggle: file references
    pub history_record_files: bool,
    /// History sort: "recent" (most recent first) / "frequent" (highest copy
    /// count first)
    pub history_sort: String,
    /// Hotkey that opens the history panel (Tauri shortcut syntax; an empty
    /// string disables it)
    pub panel_hotkey: String,
    /// Toggle for the numbered slot shortcuts (modifier+1..6) that paste
    /// directly
    pub slot_hotkeys: bool,
    /// Modifier for the slot shortcuts: "CmdOrCtrl" (⌘ on macOS, Ctrl
    /// elsewhere) / "Alt" / "Ctrl"; anything else normalizes back to
    /// CmdOrCtrl. Shared with the native client under the key `slotModifier`
    pub slot_modifier: String,
    /// Preview card delay in milliseconds: how long the highlight has to rest
    /// on an entry before the card appears; 0 is immediate. It doubles as the
    /// coalescing window while sweeping the list, so too small a value makes
    /// the card flash on a fast pass
    pub preview_delay_ms: u32,
    /// Paste automatically after an entry is chosen (macOS and Windows; needs
    /// Accessibility permission on macOS, off by default)
    ///
    /// **The key must stay `autoPaste`**: the native macOS client reads and
    /// writes the same settings.json, and this field started there. Dropping
    /// or renaming it here would wipe the native client's setting every time
    /// this one saves.
    pub auto_paste: bool,
    /// Ignore rules (started on the native macOS client; carried here so a
    /// save from this side does not wipe them — same lesson as `auto_paste`).
    /// This client does not enforce them yet: UI and pipeline hooks are a
    /// follow-up
    pub ignore: IgnoreSettings,
}

/// One ignored source application (bundle identifier plus the display name
/// frozen at add time)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IgnoredApp {
    /// Bundle identifier ("com.google.Chrome")
    pub id: String,
    /// Display name shown by the settings page
    pub name: String,
}

/// Ignore rules configuration (the `ignore` object; field-for-field identical
/// to the native client's IgnoreSettings — the two clients share the file)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IgnoreSettings {
    /// Applications whose text copies are ignored
    pub apps: Vec<IgnoredApp>,
    /// Pasteboard types that mark a change as ignored
    pub types: Vec<String>,
    /// Regular expressions matched against the whole text
    pub regexes: Vec<String>,
    /// File ignore rules, raw editor text (one simplified .gitignore pattern
    /// per line)
    pub file_patterns: String,
    /// Application rule: suppress sync
    pub apps_sync: bool,
    /// Application rule: suppress history recording
    pub apps_record: bool,
    /// Type rule: suppress sync
    pub types_sync: bool,
    /// Type rule: suppress history recording
    pub types_record: bool,
    /// Regex rule: suppress sync
    pub regex_sync: bool,
    /// Regex rule: suppress history recording
    pub regex_record: bool,
    /// File rule: suppress sync
    pub files_sync: bool,
    /// File rule: suppress history recording
    pub files_record: bool,
}

/// The default pasteboard type list (must stay identical to the native
/// client's IgnoreSettings.defaultTypes: both clients fill it in when the
/// field is absent, and differing defaults would flip-flop the file)
const DEFAULT_IGNORE_TYPES: [&str; 6] = [
    "de.petermaurer.TransientPasteboardType",
    "org.nspasteboard.TransientType",
    "org.nspasteboard.ConcealedType",
    "com.agilebits.onepassword",
    "net.antelle.keeweb",
    "com.typeit4me.clipping",
];

impl Default for IgnoreSettings {
    fn default() -> Self {
        Self {
            apps: Vec::new(),
            types: DEFAULT_IGNORE_TYPES
                .iter()
                .map(ToString::to_string)
                .collect(),
            regexes: Vec::new(),
            file_patterns: String::new(),
            apps_sync: true,
            apps_record: false,
            types_sync: true,
            types_record: false,
            regex_sync: true,
            regex_record: false,
            files_sync: true,
            files_record: false,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tcp_port: lanecho_core::DEFAULT_TCP_PORT,
            autostart: false,
            sync_enabled: true,
            sync_mode: "both".to_string(),
            sync_text: true,
            sync_images: true,
            sync_files: false,
            max_sync_file_mb: 32,
            notify_on_sync: true,
            language: String::new(),
            history_max_entries: 200,
            history_record_text: true,
            history_record_images: true,
            history_record_files: true,
            history_sort: "recent".to_string(),
            panel_hotkey: "CmdOrCtrl+Shift+C".to_string(),
            slot_hotkeys: true,
            slot_modifier: "CmdOrCtrl".to_string(),
            preview_delay_ms: 150,
            auto_paste: false,
            ignore: IgnoreSettings::default(),
        }
    }
}

impl Settings {
    /// Load from the data directory; falls back to the defaults when the file
    /// is missing or corrupt
    pub fn load(data_dir: &Path) -> Self {
        let Some(bytes) = std::fs::read(data_dir.join(SETTINGS_FILE)).ok() else {
            return Self::default();
        };
        let Some(mut settings) = serde_json::from_slice::<Self>(&bytes).ok() else {
            return Self::default();
        };
        // Migration: when an older file has no syncMode key, derive it from
        // syncEnabled. The raw JSON has to be inspected explicitly, because
        // the struct-level #[serde(default)] fills the missing field with
        // Default's "both"; skipping this step would flip a user who had
        // syncEnabled=false back on across the upgrade
        let raw: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
        let has_mode = raw
            .as_ref()
            .and_then(|v| v.get("syncMode"))
            .is_some_and(|v| v.is_string());
        if !has_mode {
            settings.sync_mode = if settings.sync_enabled { "both" } else { "off" }.to_string();
        }
        settings.normalize();
        settings
    }

    /// Normalization rules: an unknown syncMode falls back to both (logged),
    /// syncEnabled is double-written, and the file cap is clamped to 1~512.
    /// Applied before every save and after every load, so what sits on disk is
    /// always canonical
    pub fn normalize(&mut self) {
        if lanecho_core::sync::SyncMode::parse(&self.sync_mode).is_none() {
            tracing::warn!(mode = %self.sync_mode, "Unknown syncMode; falling back to both");
            self.sync_mode = "both".to_string();
        }
        self.sync_enabled = self.sync_mode != "off";
        self.max_sync_file_mb = self.max_sync_file_mb.clamp(1, 512);
        if !matches!(self.slot_modifier.as_str(), "CmdOrCtrl" | "Alt" | "Ctrl") {
            tracing::warn!(modifier = %self.slot_modifier, "Unknown slotModifier; falling back to CmdOrCtrl");
            self.slot_modifier = "CmdOrCtrl".to_string();
        }
    }

    /// Persist to the data directory with an atomic write: an in-place
    /// overwrite that gets interrupted leaves truncated JSON, and the next
    /// load silently returns the defaults, quietly resetting hotkeys, toggles
    /// and language
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = data_dir.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, data_dir.join(SETTINGS_FILE))
    }
}

/// Whether any history record type went from off to on (recording resumed),
/// which means the watcher's dedup baseline has to be reset
///
/// Content copied while recording was paused is already in the watcher's
/// last_hash even though the history layer skipped it; copying that same
/// content again after resuming is swallowed by dedup (no event, so still no
/// record) until something else displaces the baseline. From the user's side
/// that reads as "I ticked it back on and it still records nothing".
pub fn record_resumed(old: &Settings, new: &Settings) -> bool {
    (!old.history_record_text && new.history_record_text)
        || (!old.history_record_images && new.history_record_images)
        || (!old.history_record_files && new.history_record_files)
}

/// Whether a sync pipeline went from off to on (sending resumed), which
/// likewise means the watcher's dedup baseline has to be reset
///
/// Content copied while the gate was closed is already in the watcher baseline
/// even though the broadcast was blocked; copying it again after reopening
/// never goes out unless the baseline is reset. Turn text sync off, copy "7",
/// turn it back on, and copying "7" again will not sync no matter what.
///
/// Only the **send** side counts: the type toggles are bidirectional, but a
/// gap on the receive side lives on the peer (its baseline is the one holding
/// that content), and resetting locally cannot repair it.
pub fn sync_resumed(old: &Settings, new: &Settings) -> bool {
    let sends = |s: &Settings| s.sync_mode == "both" || s.sync_mode == "send";
    (!sends(old) && sends(new))
        || (!old.sync_text && new.sync_text)
        || (!old.sync_images && new.sync_images)
        || (!old.sync_files && new.sync_files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a JSON file into a temp directory and load it back
    fn load_from_json(json: &str) -> Settings {
        let dir = std::env::temp_dir().join(format!("lanecho-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SETTINGS_FILE), json).unwrap();
        let settings = Settings::load(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        settings
    }

    /// Migration golden sample: the old shape (no syncMode) derives from
    /// syncEnabled and must not be flipped to both by the struct-level serde
    /// default
    #[test]
    fn legacy_sync_enabled_false_migrates_to_off() {
        let settings = load_from_json(r#"{"syncEnabled":false}"#);
        assert_eq!(settings.sync_mode, "off");
        assert!(!settings.sync_enabled);
    }

    /// A user on the old shape with sync on migrates to both
    #[test]
    fn legacy_sync_enabled_true_migrates_to_both() {
        let settings = load_from_json(r#"{"syncEnabled":true}"#);
        assert_eq!(settings.sync_mode, "both");
        assert!(settings.sync_enabled);
    }

    /// The new shape wins: where the two fields disagree, syncMode is the
    /// source of truth and the double-write is repaired from it
    #[test]
    fn sync_mode_wins_over_stale_sync_enabled() {
        let settings = load_from_json(r#"{"syncEnabled":false,"syncMode":"send"}"#);
        assert_eq!(settings.sync_mode, "send");
        assert!(
            settings.sync_enabled,
            "Double-write rule: mode != off implies enabled"
        );
    }

    /// An unknown syncMode falls back to both; the file cap is clamped to
    /// 1~512
    #[test]
    fn normalize_clamps_and_falls_back() {
        let settings = load_from_json(r#"{"syncMode":"sideways","maxSyncFileMb":9999}"#);
        assert_eq!(settings.sync_mode, "both");
        assert_eq!(settings.max_sync_file_mb, 512);
        let settings = load_from_json(r#"{"maxSyncFileMb":0}"#);
        assert_eq!(settings.max_sync_file_mb, 1);
    }

    /// Cross-client golden sample: the native macOS client owns this file too
    /// and `autoPaste` started there, so the key has to survive a load/save
    /// round trip untouched. Losing it would silently reset the native
    /// client's setting on every save from this side
    #[test]
    fn auto_paste_survives_a_round_trip_with_the_native_client() {
        let settings = load_from_json(r#"{"autoPaste":true}"#);
        assert!(
            settings.auto_paste,
            "autoPaste must decode from the shared file"
        );

        let json = serde_json::to_string(&settings).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            raw.get("autoPaste"),
            Some(&serde_json::Value::Bool(true)),
            "The key the native client reads must be spelled autoPaste"
        );
    }

    /// Default values used when the newer fields are absent
    #[test]
    fn defaults_match_plan() {
        let settings = Settings::default();
        assert_eq!(settings.sync_mode, "both");
        assert!(settings.sync_text);
        assert!(settings.sync_images);
        assert!(!settings.sync_files);
        assert_eq!(settings.max_sync_file_mb, 32);
        assert_eq!(settings.panel_hotkey, "CmdOrCtrl+Shift+C");
        assert_eq!(
            settings.slot_modifier, "CmdOrCtrl",
            "The default slot modifier resolves to Cmd on macOS and Ctrl on Windows"
        );
    }

    /// Cross-client golden sample for the ignore rules: the native macOS
    /// client owns this object; a load/save round trip from this side must
    /// keep every nested key intact (camelCase spelling included), and an
    /// absent object fills in the shared default type list
    #[test]
    fn ignore_rules_survive_a_round_trip_with_the_native_client() {
        let settings = load_from_json(
            r#"{"ignore":{"apps":[{"id":"com.google.Chrome","name":"Chrome"}],
                "types":["custom.type"],"regexes":["^secret$"],
                "filePatterns":"*.key","appsSync":false,"appsRecord":true}}"#,
        );
        assert_eq!(settings.ignore.apps.len(), 1);
        assert_eq!(settings.ignore.apps[0].id, "com.google.Chrome");
        assert_eq!(settings.ignore.types, vec!["custom.type"]);
        assert_eq!(settings.ignore.regexes, vec!["^secret$"]);
        assert_eq!(settings.ignore.file_patterns, "*.key");
        assert!(!settings.ignore.apps_sync, "Stored toggles must win");
        assert!(settings.ignore.apps_record);
        assert!(
            settings.ignore.types_sync,
            "Absent toggles take the defaults"
        );

        let json = serde_json::to_string(&settings).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ignore = raw.get("ignore").expect("the ignore object must persist");
        for key in [
            "apps",
            "types",
            "regexes",
            "filePatterns",
            "appsSync",
            "appsRecord",
            "typesSync",
            "typesRecord",
            "regexSync",
            "regexRecord",
            "filesSync",
            "filesRecord",
        ] {
            assert!(
                ignore.get(key).is_some(),
                "The native client reads camelCase key {key}"
            );
        }

        // An absent object fills in the shared defaults, type list included
        let settings = load_from_json("{}");
        assert_eq!(settings.ignore.types.len(), 6);
        assert!(settings.ignore.types_sync);
        assert!(!settings.ignore.types_record);
    }

    /// Cross-client golden sample for the slot modifier: the key is spelled
    /// `slotModifier` on both sides of the shared file, a stored value
    /// survives a round trip, and an unknown value normalizes to CmdOrCtrl
    /// instead of producing an unparsable shortcut
    #[test]
    fn slot_modifier_round_trips_and_normalizes() {
        let settings = load_from_json(r#"{"slotModifier":"Alt"}"#);
        assert_eq!(settings.slot_modifier, "Alt");

        let json = serde_json::to_string(&settings).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            raw.get("slotModifier"),
            Some(&serde_json::Value::String("Alt".into())),
            "The key the native client reads must be spelled slotModifier"
        );

        let settings = load_from_json(r#"{"slotModifier":"Hyper"}"#);
        assert_eq!(settings.slot_modifier, "CmdOrCtrl");
    }

    /// Record-resumed decision: true when any type goes off→on; false when a
    /// type is turned off and when nothing changed
    #[test]
    fn record_resumed_detects_reenabling() {
        let new = Settings::default();
        assert!(!record_resumed(&new, &new), "No change must not trigger");

        let text_off = Settings {
            history_record_text: false,
            ..Default::default()
        };
        assert!(
            record_resumed(&text_off, &new),
            "Re-enabling text must trigger"
        );
        assert!(
            !record_resumed(&new, &text_off),
            "Disabling a direction must not trigger"
        );

        let files_off = Settings {
            history_record_files: false,
            ..Default::default()
        };
        assert!(
            record_resumed(&files_off, &new),
            "Re-enabling files must trigger"
        );
    }

    /// Sync-resumed decision: true when a type goes off→on or the direction
    /// gains send capability; false when turning off, when nothing changed,
    /// and when switching between modes that never send
    #[test]
    fn sync_resumed_detects_pipeline_reopening() {
        let new = Settings::default();
        assert!(!sync_resumed(&new, &new), "No change must not trigger");

        let text_off = Settings {
            sync_text: false,
            ..Default::default()
        };
        assert!(
            sync_resumed(&text_off, &new),
            "Re-enabling text sync must trigger"
        );
        assert!(
            !sync_resumed(&new, &text_off),
            "Disabling a direction must not trigger"
        );

        let mode_off = Settings {
            sync_mode: "off".to_string(),
            ..Default::default()
        };
        assert!(
            sync_resumed(&mode_off, &new),
            "off -> both gains send capability and must trigger"
        );

        let recv = Settings {
            sync_mode: "receive".to_string(),
            ..Default::default()
        };
        let send = Settings {
            sync_mode: "send".to_string(),
            ..Default::default()
        };
        assert!(
            sync_resumed(&recv, &send),
            "receive -> send gains send capability and must trigger"
        );
        assert!(
            !sync_resumed(&send, &new),
            "send -> both does not change send capability"
        );
        assert!(
            !sync_resumed(&mode_off, &recv),
            "off -> receive still cannot send and must not trigger"
        );
    }
}
