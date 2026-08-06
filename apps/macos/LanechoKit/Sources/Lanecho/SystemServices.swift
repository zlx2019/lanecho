// System capability wrappers: notifications / launch at login / local network
// permission onboarding.
//
// All three need a real .app bundle — the bare `swift run` binary has no
// bundle id, UNUserNotificationCenter throws outright and SMAppService has
// nothing to register. Each short-circuits on `isBundled` so development
// builds degrade silently instead of crashing.

import AppKit
import ServiceManagement
import UserNotifications

/// System notifications
///
/// **Use only the async variants of the notification center, never the
/// completion-handler APIs**: a non-@Sendable completion closure formed in a
/// @MainActor context is inferred by Swift 6 as MainActor-isolated, and the
/// compiler plants a queue assertion at the closure entry; the callbacks of
/// UNUserNotificationCenter run on a private queue, so
/// _dispatch_assert_queue_fail takes the process down. Whether that assertion
/// is emitted depends on the toolchain — an installed build crashes on its
/// first notification while a development build cannot reproduce it at all.
/// The async variants have no closure, so no toolchain can make that
/// inference.
@MainActor
enum Notifications {
    /// Whether authorization has been requested; once per session is enough
    private static var requested = false

    /// Notification for an arriving remote sync ("synced from X" plus a
    /// content summary)
    static func syncArrived(from device: String, preview: String) {
        guard isBundled else { return }
        Task {
            guard await ensureGranted() else { return }
            let content = UNMutableNotificationContent()
            content.title = L10n.t.notifySyncTitle(device)
            content.body = preview
            content.sound = nil
            let request = UNNotificationRequest(
                identifier: UUID().uuidString, content: content, trigger: nil)
            try? await UNUserNotificationCenter.current().add(request)
        }
    }

    /// Request or query authorization; the await hops back to MainActor on
    /// its own, so there is no callback-thread problem
    private static func ensureGranted() async -> Bool {
        if requested {
            return await queryAuthorized()
        }
        requested = true
        let center = UNUserNotificationCenter.current()
        return (try? await center.requestAuthorization(options: [.alert])) ?? false
    }

    /// Current authorization state (nonisolated because UNNotificationSettings
    /// is not Sendable and carrying it back to MainActor fails the concurrency
    /// check; reduce it to a Bool in the non-isolated context instead)
    private nonisolated static func queryAuthorized() async -> Bool {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        return settings.authorizationStatus == .authorized
    }
}

/// Launch at login (login item)
@MainActor
enum LoginItem {
    /// Whether we are currently registered as a login item
    static var isEnabled: Bool {
        guard isBundled else { return false }
        return SMAppService.mainApp.status == .enabled
    }

    /// Set the login item; false on failure — the system refuses if the user
    /// disabled it in System Settings
    @discardableResult
    static func setEnabled(_ enabled: Bool) -> Bool {
        guard isBundled else { return false }
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            return true
        } catch {
            return false
        }
    }
}

/// Local network permission onboarding. On macOS 15+ the system only asks on
/// the first multicast/Bonjour use, and once denied discovery fails silently —
/// all the user sees is a broken app.
@MainActor
enum LocalNetworkOnboarding {
    /// Whether onboarding has been shown; macOS-only UI state, kept in
    /// UserDefaults rather than settings.json so it cannot collide with the
    /// Tauri client's schema
    private static let seenKey = "io.github.zlx2019.lanecho.seenLocalNetworkOnboarding"

    /// Show it once, on first launch
    static func presentIfNeeded() {
        guard isBundled, !UserDefaults.standard.bool(forKey: seenKey) else { return }
        UserDefaults.standard.set(true, forKey: seenKey)
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = L10n.t.localNetworkTitle
        alert.informativeText = L10n.t.localNetworkBody
        alert.addButton(withTitle: L10n.t.gotIt)
        alert.addButton(withTitle: L10n.t.openSystemSettings)
        if alert.runModal() == .alertSecondButtonReturn {
            openLocalNetworkSettings()
        }
    }

    /// Open the Local Network privacy pane in System Settings
    static func openLocalNetworkSettings() {
        guard
            let url = URL(
                string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork"
            )
        else { return }
        NSWorkspace.shared.open(url)
    }
}
