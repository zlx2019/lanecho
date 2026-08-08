// Settings file interop tests: settings.json is read and written by both this
// build and the Tauri build

import Foundation
import Testing

@testable import LanechoKit

/// Temporary data directory
private func tempDir() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-settings-\(UUID().uuidString)")
}

/// settings.json written by the Tauri build must load as is: it does not know
/// autoPaste and drops the field on save, so a missing field falls back to its
/// default instead of failing to decode
@Test func settingsReadsTauriFile() throws {
    let dir = tempDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    // The exact shape Rust serde emits: camelCase, all fields present, no
    // autoPaste
    let tauri = """
        {"tcpPort":42524,"autostart":false,"syncEnabled":true,"notifyOnSync":true,\
        "language":"zh","historyMaxEntries":200,"historyRecordText":true,\
        "historyRecordImages":true,"historyRecordFiles":true,"historySort":"recent",\
        "panelHotkey":"CmdOrCtrl+Shift+V","slotHotkeys":true,"previewDelayMs":150}
        """
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    try Data(tauri.utf8).write(to: dir.appendingPathComponent("settings.json"))

    let settings = Settings.load(dataDir: dir)
    #expect(settings.tcpPort == 42524)
    #expect(settings.language == "zh")
    #expect(settings.previewDelayMs == 150)
    #expect(settings.autoPaste == false, "Fields absent from the Tauri build must use defaults")
}

/// An empty or corrupt file yields the defaults; broken settings must not keep
/// the app from starting
@Test func settingsFallsBackToDefaults() throws {
    let dir = tempDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    try Data("{ not json".utf8).write(to: dir.appendingPathComponent("settings.json"))
    #expect(Settings.load(dataDir: dir) == Settings())
}

/// Disk round trip: the fields the native build adds do persist, and key names
/// stay camelCase
@Test func settingsRoundtripsWithNativeFields() throws {
    let dir = tempDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    var settings = Settings()
    settings.autoPaste = true
    settings.language = "en"
    try settings.save(dataDir: dir)

    let text = try String(
        contentsOf: dir.appendingPathComponent("settings.json"), encoding: .utf8)
    #expect(text.contains("\"autoPaste\""))
    #expect(text.contains("\"previewDelayMs\""))
    #expect(text.contains("\"slotModifier\""))
    #expect(Settings.load(dataDir: dir) == settings)
}

/// Ignore rules: defaults carry the shared type list with sync suppressed and
/// recording kept; stored values survive a decode; a partial object fills the
/// gaps with defaults (serde default semantics)
@Test func ignoreSettingsDefaultsAndDecoding() throws {
    let defaults = IgnoreSettings()
    #expect(defaults.types == IgnoreSettings.defaultTypes)
    #expect(defaults.types.count == 6)
    #expect(defaults.appsSync && defaults.typesSync && defaults.regexSync && defaults.filesSync)
    #expect(
        !defaults.appsRecord && !defaults.typesRecord && !defaults.regexRecord
            && !defaults.filesRecord)

    let json = """
        {"apps":[{"id":"com.google.Chrome","name":"Chrome"}],"types":["custom.type"],\
        "regexes":["^secret$"],"filePatterns":"*.key","appsSync":false,"appsRecord":true}
        """
    let decoded = try JSONDecoder().decode(IgnoreSettings.self, from: Data(json.utf8))
    #expect(decoded.apps == [IgnoredApp(id: "com.google.Chrome", name: "Chrome")])
    #expect(decoded.types == ["custom.type"])
    #expect(decoded.regexes == ["^secret$"])
    #expect(decoded.filePatterns == "*.key")
    #expect(!decoded.appsSync && decoded.appsRecord)
    #expect(decoded.typesSync && !decoded.typesRecord, "Absent toggles take the defaults")
}

/// Ignore rules ride settings.json round trips (the nested object is what the
/// Tauri client carries as a passthrough)
@Test func ignoreSettingsRoundtripInSettingsFile() throws {
    let dir = tempDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    var settings = Settings()
    settings.ignore.apps = [IgnoredApp(id: "com.apple.dt.Xcode", name: "Xcode")]
    settings.ignore.regexes = ["^[a-zA-Z0-9]{50}$"]
    settings.ignore.typesRecord = true
    try settings.save(dataDir: dir)

    let text = try String(
        contentsOf: dir.appendingPathComponent("settings.json"), encoding: .utf8)
    #expect(text.contains("\"ignore\""))
    #expect(text.contains("\"filePatterns\""))
    #expect(Settings.load(dataDir: dir) == settings)
}

/// Slot modifier: a stored choice survives, an unknown value normalizes back
/// to CmdOrCtrl (same whitelist as the Tauri client) instead of producing an
/// unregistrable shortcut
@Test func slotModifierNormalizes() {
    #expect(Settings().slotModifier == "CmdOrCtrl")
    var settings = Settings()
    settings.slotModifier = "Alt"
    settings.normalize()
    #expect(settings.slotModifier == "Alt")
    settings.slotModifier = "Hyper"
    settings.normalize()
    #expect(settings.slotModifier == "CmdOrCtrl")
}

/// Write a legacy-shaped file: no sync v2 fields at all, only syncEnabled
private func writeLegacy(syncEnabled: Bool, to dir: URL) throws {
    let legacy = """
        {"tcpPort":42524,"autostart":false,"syncEnabled":\(syncEnabled),\
        "notifyOnSync":true,"language":"zh","historyMaxEntries":200,\
        "historyRecordText":true,"historyRecordImages":true,\
        "historyRecordFiles":true,"historySort":"recent",\
        "panelHotkey":"CmdOrCtrl+Shift+V","slotHotkeys":true,"previewDelayMs":150}
        """
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    try Data(legacy.utf8).write(to: dir.appendingPathComponent("settings.json"))
}

/// **Migration golden sample**: when a legacy file has no syncMode, derive it
/// from syncEnabled — defaulting straight to "both" would silently flip the
/// configuration of users who had sync turned off
@Test func settingsMigratesLegacySyncEnabled() throws {
    let dir = tempDir()
    defer { try? FileManager.default.removeItem(at: dir) }

    try writeLegacy(syncEnabled: false, to: dir)
    let off = Settings.load(dataDir: dir)
    #expect(off.syncMode == "off", "Legacy syncEnabled=false must migrate to off")
    #expect(off.syncEnabled == false)
    #expect(off.engineSyncMode == .off)

    try writeLegacy(syncEnabled: true, to: dir)
    let both = Settings.load(dataDir: dir)
    #expect(both.syncMode == "both")
    #expect(both.engineSyncTypes == SyncTypes(), "Missing type toggles must use defaults with files off")
    #expect(both.maxSyncFileMb == 32)
}

/// Normalize: an unknown mode falls back to both, syncEnabled is
/// double-written, the limit is clamped into 1~512
@Test func settingsNormalizesUnknownModeAndClampsLimit() throws {
    let dir = tempDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let weird = """
        {"syncEnabled":false,"syncMode":"sideways","maxSyncFileMb":99999}
        """
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    try Data(weird.utf8).write(to: dir.appendingPathComponent("settings.json"))

    let settings = Settings.load(dataDir: dir)
    #expect(settings.syncMode == "both", "An unknown mode must fall back to both")
    #expect(settings.syncEnabled == true, "syncEnabled must follow the normalized mode")
    #expect(settings.maxSyncFileMb == 512)

    // Double-write on save: once mode=off is persisted, syncEnabled must be
    // false, because older versions read that field
    var off = settings
    off.syncMode = "off"
    try off.save(dataDir: dir)
    let raw = try Data(contentsOf: dir.appendingPathComponent("settings.json"))
    let json = try #require(
        try JSONSerialization.jsonObject(with: raw) as? [String: Any])
    #expect(json["syncEnabled"] as? Bool == false)
    #expect(json["syncMode"] as? String == "off")
}

/// Record-resumed decision: true when any type goes off→on; false when one is
/// turned off and when nothing changed (this is what resets the watcher's
/// dedup baseline — content copied while recording was paused must still be
/// recorded when it is copied again after resuming)
@Test func recordResumedDetectsReenabling() {
    var old = Settings()
    let new = Settings()
    #expect(!recordResumed(old: old, new: new), "No change must not trigger")

    old.historyRecordText = false
    #expect(recordResumed(old: old, new: new), "Re-enabling text must trigger")

    var off = Settings()
    off.historyRecordFiles = false
    #expect(!recordResumed(old: new, new: off), "Disabling a direction must not trigger")
}

/// Sync-resumed decision: true when a type goes off→on or the direction gains
/// send capability; false when one is turned off, when nothing changed, and
/// for switches that still do not send
@Test func syncResumedDetectsPipelineReopening() {
    let new = Settings()
    #expect(!syncResumed(old: new, new: new), "No change must not trigger")

    var textOff = Settings()
    textOff.syncText = false
    #expect(syncResumed(old: textOff, new: new), "Re-enabling text sync must trigger")
    #expect(!syncResumed(old: new, new: textOff), "Disabling a direction must not trigger")

    var modeOff = Settings()
    modeOff.syncMode = "off"
    #expect(syncResumed(old: modeOff, new: new), "off -> both gains send capability and must trigger")

    var recv = Settings()
    recv.syncMode = "receive"
    var send = Settings()
    send.syncMode = "send"
    #expect(syncResumed(old: recv, new: send), "receive -> send gains send capability and must trigger")
    #expect(!syncResumed(old: send, new: new), "send -> both does not change send capability")
    #expect(!syncResumed(old: modeOff, new: recv), "off -> receive still cannot send and must not trigger")
}
