// Settings window view model: bridges AppCore's settings / devices / storage
// command surface.
//
// Save granularity (same as the Tauri client): toggles and pickers persist on
// every change (patch), text fields (display name / port) commit through a
// save action; all of them persist first and apply side effects only after the
// write succeeds, rolling the displayed value back when it fails.

import AppKit
import LanechoKit
import Observation
import UniformTypeIdentifiers

/// Settings window view model
@MainActor
@Observable
final class SettingsModel {
    private let core: AppCore
    private let hotkeys: HotkeyCoordinator
    /// Language-change callback (AppKit parts such as the panel rebuild their text)
    private let onLanguageChanged: () -> Void

    /// Current settings snapshot (the editing baseline)
    var settings = Settings()
    /// Current text table (a language change applies immediately; views read this)
    var texts = L10n.t
    /// Display name field
    var deviceName = ""
    /// Port field
    var portText = ""
    /// File sync limit field (MB)
    var fileLimitText = ""
    /// Preview card delay field (ms)
    var previewDelayText = ""
    /// Local fingerprint (display only)
    var localFingerprint = ""
    /// Incognito (session state)
    var incognito = false
    /// Launch at login (source of truth is the system login item, not settings.json)
    var autostart = false
    /// Whether launch at login is available (it is not when running unbundled)
    let autostartAvailable = isBundled
    /// Device rows (online ∪ paired)
    var devices: [DeviceRow] = []
    /// History disk usage
    var usageBytes: UInt64 = 0
    /// Hotkeys that failed to register
    var hotkeyFailures: [String] = []
    /// Fingerprints with a pairing in flight
    var pairingBusy: Set<String> = []
    /// Callback for dynamic in-page content (the window re-measures its height
    /// when the conflict list appears or disappears)
    var onContentSizeChanged: (() -> Void)?

    /// One row of the device list
    struct DeviceRow: Identifiable, Equatable {
        /// Fingerprint
        var id: String
        var name: String
        var platform: String
        var online: Bool
        var paired: Bool
    }

    init(core: AppCore, hotkeys: HotkeyCoordinator, onLanguageChanged: @escaping () -> Void) {
        self.core = core
        self.hotkeys = hotkeys
        self.onLanguageChanged = onLanguageChanged
    }

    /// Last saved display name (keeps a blur commit from rewriting an unchanged value)
    private var savedDeviceName = ""

    /// Full load when the window comes up
    func load() async {
        settings = await core.currentSettings()
        let info = await core.localInfo()
        deviceName = info.name
        savedDeviceName = info.name
        localFingerprint = info.fingerprint
        portText = String(settings.tcpPort)
        fileLimitText = String(settings.maxSyncFileMb)
        previewDelayText = String(settings.previewDelayMs)
        incognito = await core.isIncognito()
        autostart = LoginItem.isEnabled
        usageBytes = await core.historyDiskUsage()
        hotkeyFailures = hotkeys.failures
        await refreshDevices()
    }

    /// Re-fetch the device list (event driven, plus once on open)
    func refreshDevices() async {
        let peers = await core.peers()
        let paired = await core.pairedList()
        var rows: [DeviceRow] = []
        var seen = Set<String>()
        for peer in peers {
            seen.insert(peer.info.fingerprint)
            rows.append(
                DeviceRow(
                    id: peer.info.fingerprint, name: peer.info.name,
                    platform: peer.info.platform, online: true,
                    paired: paired.contains { $0.fingerprint == peer.info.fingerprint }))
        }
        for entry in paired where !seen.contains(entry.fingerprint) {
            rows.append(
                DeviceRow(
                    id: entry.fingerprint, name: entry.name, platform: "",
                    online: false, paired: true))
        }
        devices = rows
    }

    /// UI events (forwarded by the shell layer pump)
    func handle(_ event: CoreEvent) {
        switch event {
        case .peerUp, .peerDown, .paired, .unpaired:
            Task { await refreshDevices() }
        case .historyChanged:
            Task { usageBytes = await core.historyDiskUsage() }
        default:
            break
        }
    }

    // MARK: - Saving

    /// Toggles and pickers: persist on every change; roll the display back if
    /// the write fails
    func patch(_ mutate: (inout Settings) -> Void) {
        let previous = settings
        var next = settings
        mutate(&next)
        guard next != previous else { return }
        settings = next
        Task {
            do {
                try await core.updateSettings(next)
            } catch {
                settings = previous
                NSSound.beep()
            }
        }
    }

    /// Language switch: persist, then swap the text across the whole UI
    func changeLanguage(_ language: String) {
        patch { $0.language = language }
        L10n.apply(language: language)
        texts = L10n.t
        onLanguageChanged()
    }

    /// Save the display name (commits on return or blur, like "Computer Name"
    /// in System Settings; empty goes back to following the host name; an
    /// unchanged value is not written)
    func saveName() {
        let name = deviceName.trimmingCharacters(in: .whitespaces)
        guard name != savedDeviceName else { return }
        Task {
            do {
                try await core.setDisplayName(name.isEmpty ? nil : name)
                let info = await core.localInfo()
                deviceName = info.name
                savedDeviceName = info.name
            } catch {
                deviceName = savedDeviceName
                NSSound.beep()
            }
        }
    }

    /// Save the port (takes effect after a restart); invalid input rolls the
    /// display back
    func savePort() {
        guard let port = UInt16(portText.trimmingCharacters(in: .whitespaces)) else {
            portText = String(settings.tcpPort)
            NSSound.beep()
            return
        }
        patch { $0.tcpPort = port }
    }

    /// Save the file sync limit (MB, clamped to 1~512); invalid input rolls the
    /// display back
    func saveFileLimit() {
        guard let raw = UInt32(fileLimitText.trimmingCharacters(in: .whitespaces)), raw > 0
        else {
            fileLimitText = String(settings.maxSyncFileMb)
            NSSound.beep()
            return
        }
        let clamped = min(max(raw, 1), 512)
        patch { $0.maxSyncFileMb = clamped }
        fileLimitText = String(clamped)
    }

    /// Save the preview card delay (ms, clamped to 0~5000 — the same range
    /// the Tauri client accepts); invalid input rolls the display back
    func savePreviewDelay() {
        guard let raw = UInt32(previewDelayText.trimmingCharacters(in: .whitespaces)) else {
            previewDelayText = String(settings.previewDelayMs)
            NSSound.beep()
            return
        }
        let clamped = min(raw, 5000)
        patch { $0.previewDelayMs = clamped }
        previewDelayText = String(clamped)
    }

    /// Rebind the panel hotkey (recorded or cleared)
    func changePanelHotkey(_ accelerator: String) {
        patch { $0.panelHotkey = accelerator }
        hotkeys.reregister(settings: settings)
        hotkeyFailures = hotkeys.failures
        onContentSizeChanged?()
    }

    /// Slot direct-paste toggle
    func changeSlotHotkeys(_ on: Bool) {
        patch { $0.slotHotkeys = on }
        hotkeys.reregister(settings: settings)
        hotkeyFailures = hotkeys.failures
        onContentSizeChanged?()
    }

    /// Slot modifier choice (⌘/⌥/⌃); re-registers, since the new combination
    /// may collide with another app
    func changeSlotModifier(_ modifier: String) {
        patch { $0.slotModifier = modifier }
        hotkeys.reregister(settings: settings)
        hotkeyFailures = hotkeys.failures
        onContentSizeChanged?()
    }

    /// Incognito toggle (session state, never persisted)
    func toggleIncognito(_ on: Bool) {
        incognito = on
        Task { await core.setIncognito(on) }
    }

    /// Auto-paste toggle: turning it on must check the permission right here —
    /// without Accessibility access the synthesized keystroke is silently
    /// dropped by the system, so the switch does nothing
    func toggleAutoPaste(_ on: Bool) {
        guard on else {
            patch { $0.autoPaste = false }
            return
        }
        patch { $0.autoPaste = true }
        guard !AutoPaste.isTrusted else { return }
        AutoPaste.requestTrust(prompt: true)
        let alert = NSAlert()
        alert.messageText = texts.autoPastePermissionTitle
        alert.informativeText = texts.autoPastePermissionBody
        alert.addButton(withTitle: texts.openSystemSettings)
        alert.addButton(withTitle: texts.gotIt)
        if alert.runModal() == .alertFirstButtonReturn {
            AutoPaste.openAccessibilitySettings()
        }
    }

    /// Launch at login: register or unregister the system login item; on
    /// failure (the user disabled it in System Settings) roll the display back.
    /// The autostart field in settings.json follows along so both clients read
    /// the same value.
    func toggleAutostart(_ on: Bool) {
        guard autostartAvailable else { return }
        guard LoginItem.setEnabled(on) else {
            autostart = !on
            NSSound.beep()
            return
        }
        autostart = on
        patch { $0.autostart = on }
    }

    // MARK: - Ignore rules

    /// Add an application through the system open panel (reads the bundle
    /// identifier; an app without one, or one already listed, is a no-op)
    func addIgnoredApp() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.applicationBundle]
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        guard panel.runModal() == .OK, let url = panel.url,
            let bundle = Bundle(url: url), let id = bundle.bundleIdentifier
        else { return }
        guard !settings.ignore.apps.contains(where: { $0.id == id }) else { return }
        let name =
            (bundle.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (bundle.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? url.deletingPathExtension().lastPathComponent
        patch { $0.ignore.apps.append(IgnoredApp(id: id, name: name)) }
    }

    /// Remove one ignored application
    func removeIgnoredApp(id: String) {
        patch { $0.ignore.apps.removeAll { $0.id == id } }
    }

    /// Add a pasteboard type (trimmed; empty or duplicate is a no-op)
    func addIgnoredType(_ raw: String) {
        let type = raw.trimmingCharacters(in: .whitespaces)
        guard !type.isEmpty, !settings.ignore.types.contains(type) else { return }
        patch { $0.ignore.types.append(type) }
    }

    /// Remove one pasteboard type
    func removeIgnoredType(_ type: String) {
        patch { $0.ignore.types.removeAll { $0 == type } }
    }

    /// Restore the preset pasteboard type list
    func resetIgnoredTypes() {
        patch { $0.ignore.types = IgnoreSettings.defaultTypes }
    }

    /// Add a regex pattern (kept verbatim; empty or duplicate is a no-op —
    /// an uncompilable pattern is legal, it degrades to a literal match)
    func addIgnoredRegex(_ raw: String) {
        let pattern = raw.trimmingCharacters(in: .whitespaces)
        guard !pattern.isEmpty, !settings.ignore.regexes.contains(pattern) else { return }
        patch { $0.ignore.regexes.append(pattern) }
    }

    /// Remove one regex pattern
    func removeIgnoredRegex(_ pattern: String) {
        patch { $0.ignore.regexes.removeAll { $0 == pattern } }
    }

    /// Commit the file pattern text (on blur, like the port field)
    func saveFilePatterns(_ text: String) {
        guard text != settings.ignore.filePatterns else { return }
        patch { $0.ignore.filePatterns = text }
    }

    // MARK: - Device actions

    /// Start pairing (waits for the peer to confirm, up to 300s); on failure an
    /// alert gives a readable reason (the settings window is a regular window,
    /// so a system alert is safe here — the panel's hide-on-blur constraint
    /// does not apply)
    func pair(fingerprint: String) {
        pairingBusy.insert(fingerprint)
        Task {
            do {
                try await core.pair(fingerprint: fingerprint)
            } catch {
                let alert = NSAlert()
                alert.messageText = texts.pairFailedTitle
                alert.informativeText = texts.transportErrorText(error)
                alert.addButton(withTitle: texts.gotIt)
                alert.runModal()
            }
            pairingBusy.remove(fingerprint)
            await refreshDevices()
        }
    }

    /// Unpair
    func unpair(fingerprint: String) {
        Task {
            await core.unpair(fingerprint: fingerprint)
            await refreshDevices()
        }
    }

    // MARK: - Storage actions

    /// Clear history (the settings window is a regular window, so a system
    /// confirmation dialog is fine; the view side owns the confirmation)
    func clearHistory() {
        Task {
            await core.clearHistory()
            usageBytes = await core.historyDiskUsage()
        }
    }
}
