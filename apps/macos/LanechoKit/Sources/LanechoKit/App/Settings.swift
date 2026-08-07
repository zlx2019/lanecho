// Settings model (mirrors the Tauri client's settings.rs; both clients share
// the same settings.json)
//
// serde contract: camelCase keys, and a missing field takes its default, so
// added fields stay backward compatible. panelHotkey stores a Tauri hotkey
// syntax string (read by both clients; the native client parses it into
// NSEvent modifiers in its UI layer).

import Foundation

/// Application settings (camelCase serialization; the Tauri client reads and
/// writes the same file)
public struct Settings: Codable, Sendable, Equatable {
    /// TCP listen port (0 means random; takes effect after a restart)
    public var tcpPort: UInt16
    /// Launch at login
    public var autostart: Bool
    /// Coarse sync switch, kept and **double-written** as `syncMode != "off"`
    /// so older versions and downgrade paths still read correct behaviour
    public var syncEnabled: Bool
    /// Sync direction policy: "off" / "both" / "send" / "receive" (unknown
    /// values fall back to both)
    public var syncMode: String
    /// Sync type toggle: text (constrains send and receive alike; one toggle,
    /// bidirectional semantics)
    public var syncText: Bool
    /// Sync type toggle: images
    public var syncImages: Bool
    /// Sync type toggle: files
    public var syncFiles: Bool
    /// Total cap for one file sync (MB, 1–512)
    public var maxSyncFileMb: UInt32
    /// Post a system notification when a remote overwrites the clipboard
    public var notifyOnSync: Bool
    /// UI language: "zh" / "en"; empty means uninitialized (first launch
    /// detects the system language)
    public var language: String
    /// Cap on retained history entries
    public var historyMaxEntries: Int
    /// History record type toggle: text
    public var historyRecordText: Bool
    /// History record type toggle: images
    public var historyRecordImages: Bool
    /// History record type toggle: file references
    public var historyRecordFiles: Bool
    /// History sort: "recent" / "frequent"
    public var historySort: String
    /// Hotkey that opens the history panel (Tauri syntax; empty string
    /// disables it)
    public var panelHotkey: String
    /// Toggle for pasting directly from a numbered slot (Alt+1..6)
    public var slotHotkeys: Bool
    /// Preview card delay (milliseconds; doubles as the coalescing window
    /// while sweeping the list)
    public var previewDelayMs: UInt32
    /// Paste automatically on selection (needs the accessibility permission,
    /// off by default)
    ///
    /// The Tauri client carries the same field under the same key, so the two
    /// keep each other's value across a save. It did start here, though —
    /// before the Tauri client grew the setting, saving from that side dropped
    /// this key entirely.
    public var autoPaste: Bool

    /// Defaults (field for field identical to the Rust Default)
    public init(
        tcpPort: UInt16 = Config.tcpPort, autostart: Bool = false,
        syncEnabled: Bool = true, syncMode: String = "both", syncText: Bool = true,
        syncImages: Bool = true, syncFiles: Bool = false, maxSyncFileMb: UInt32 = 32,
        notifyOnSync: Bool = true, language: String = "",
        historyMaxEntries: Int = 200, historyRecordText: Bool = true,
        historyRecordImages: Bool = true, historyRecordFiles: Bool = true,
        historySort: String = "recent", panelHotkey: String = "CmdOrCtrl+Shift+V",
        slotHotkeys: Bool = true, previewDelayMs: UInt32 = 150,
        autoPaste: Bool = false
    ) {
        self.tcpPort = tcpPort
        self.autostart = autostart
        self.syncEnabled = syncEnabled
        self.syncMode = syncMode
        self.syncText = syncText
        self.syncImages = syncImages
        self.syncFiles = syncFiles
        self.maxSyncFileMb = maxSyncFileMb
        self.notifyOnSync = notifyOnSync
        self.language = language
        self.historyMaxEntries = historyMaxEntries
        self.historyRecordText = historyRecordText
        self.historyRecordImages = historyRecordImages
        self.historyRecordFiles = historyRecordFiles
        self.historySort = historySort
        self.panelHotkey = panelHotkey
        self.slotHotkeys = slotHotkeys
        self.previewDelayMs = previewDelayMs
        self.autoPaste = autoPaste
    }

    /// Lenient decoding: any missing field takes its default (serde default
    /// semantics)
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = Settings()
        tcpPort =
            try container.decodeIfPresent(UInt16.self, forKey: .tcpPort) ?? defaults.tcpPort
        autostart =
            try container.decodeIfPresent(Bool.self, forKey: .autostart) ?? defaults.autostart
        syncEnabled =
            try container.decodeIfPresent(Bool.self, forKey: .syncEnabled)
            ?? defaults.syncEnabled
        // Migration: when an old file has no syncMode, derive it from
        // syncEnabled — defaulting straight to "both" would flip the setting
        // for anyone who had syncEnabled=false
        syncMode =
            try container.decodeIfPresent(String.self, forKey: .syncMode)
            ?? (syncEnabled ? "both" : "off")
        syncText =
            try container.decodeIfPresent(Bool.self, forKey: .syncText) ?? defaults.syncText
        syncImages =
            try container.decodeIfPresent(Bool.self, forKey: .syncImages)
            ?? defaults.syncImages
        syncFiles =
            try container.decodeIfPresent(Bool.self, forKey: .syncFiles)
            ?? defaults.syncFiles
        maxSyncFileMb =
            try container.decodeIfPresent(UInt32.self, forKey: .maxSyncFileMb)
            ?? defaults.maxSyncFileMb
        notifyOnSync =
            try container.decodeIfPresent(Bool.self, forKey: .notifyOnSync)
            ?? defaults.notifyOnSync
        language =
            try container.decodeIfPresent(String.self, forKey: .language) ?? defaults.language
        historyMaxEntries =
            try container.decodeIfPresent(Int.self, forKey: .historyMaxEntries)
            ?? defaults.historyMaxEntries
        historyRecordText =
            try container.decodeIfPresent(Bool.self, forKey: .historyRecordText)
            ?? defaults.historyRecordText
        historyRecordImages =
            try container.decodeIfPresent(Bool.self, forKey: .historyRecordImages)
            ?? defaults.historyRecordImages
        historyRecordFiles =
            try container.decodeIfPresent(Bool.self, forKey: .historyRecordFiles)
            ?? defaults.historyRecordFiles
        historySort =
            try container.decodeIfPresent(String.self, forKey: .historySort)
            ?? defaults.historySort
        panelHotkey =
            try container.decodeIfPresent(String.self, forKey: .panelHotkey)
            ?? defaults.panelHotkey
        slotHotkeys =
            try container.decodeIfPresent(Bool.self, forKey: .slotHotkeys)
            ?? defaults.slotHotkeys
        previewDelayMs =
            try container.decodeIfPresent(UInt32.self, forKey: .previewDelayMs)
            ?? defaults.previewDelayMs
        autoPaste =
            try container.decodeIfPresent(Bool.self, forKey: .autoPaste) ?? defaults.autoPaste
        normalize()
    }

    /// Normalize (runs on both the load and save paths, same duty as the Rust
    /// normalize): unknown policies fall back to both, syncEnabled is
    /// double-written, and the cap is clamped into 1–512
    public mutating func normalize() {
        if SyncMode(rawValue: syncMode) == nil {
            syncMode = "both"
        }
        syncEnabled = syncMode != "off"
        maxSyncFileMb = min(max(maxSyncFileMb, 1), 512)
    }

    /// Reads from the data directory (a missing or corrupt file yields
    /// defaults)
    public static func load(dataDir: URL) -> Settings {
        let path = dataDir.appendingPathComponent("settings.json")
        guard let bytes = try? Data(contentsOf: path),
            let settings = try? JSONDecoder().decode(Settings.self, from: bytes)
        else { return Settings() }
        return settings
    }

    /// Atomic write into the data directory (throws on failure; callers
    /// persist first and apply side effects only after the write succeeds)
    public func save(dataDir: URL) throws {
        var normalized = self
        normalized.normalize()
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        let bytes = try encoder.encode(normalized)
        try FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
        try bytes.write(to: dataDir.appendingPathComponent("settings.json"), options: .atomic)
    }

    /// Derived history recording config
    public var historyConfig: HistoryConfig {
        HistoryConfig(
            maxEntries: historyMaxEntries, recordText: historyRecordText,
            recordImages: historyRecordImages, recordFiles: historyRecordFiles)
    }

    /// Sync direction policy as the engine sees it (unknown values fall back
    /// to both, same semantics as normalize)
    public var engineSyncMode: SyncMode {
        SyncMode(rawValue: syncMode) ?? .both
    }

    /// Sync type toggles as the engine sees them
    public var engineSyncTypes: SyncTypes {
        SyncTypes(text: syncText, images: syncImages, files: syncFiles)
    }

    /// File sync cap (bytes)
    public var maxSyncFileBytes: UInt64 {
        UInt64(maxSyncFileMb) * 1024 * 1024
    }
}

/// Whether any history record type went from off to on (recording resumed),
/// which requires resetting the watcher's dedup baseline
///
/// Content copied while recording was paused already sits in the watcher's
/// lastHash even though the history layer skipped it; copying that same
/// content again after resuming is swallowed by dedup (no event → still not
/// recorded) until some other copy displaces the baseline. To the user it
/// looks like the box is ticked yet nothing gets recorded.
public func recordResumed(old: Settings, new: Settings) -> Bool {
    (!old.historyRecordText && new.historyRecordText)
        || (!old.historyRecordImages && new.historyRecordImages)
        || (!old.historyRecordFiles && new.historyRecordFiles)
}

/// Whether any sync pipe went from off to on (sending resumed), which needs
/// the same watcher dedup baseline reset
///
/// Content copied while the gate was closed is already in the baseline but its
/// broadcast was blocked; after reopening the gate, copying that same content
/// never goes out unless the baseline is reset. Only **send-side** resumption
/// counts: the type toggles are bidirectional, but a receive-side gap lives in
/// the peer's baseline and cannot be fixed from here.
public func syncResumed(old: Settings, new: Settings) -> Bool {
    func sends(_ s: Settings) -> Bool { s.syncMode == "both" || s.syncMode == "send" }
    return (!sends(old) && sends(new))
        || (!old.syncText && new.syncText)
        || (!old.syncImages && new.syncImages)
        || (!old.syncFiles && new.syncFiles)
}
