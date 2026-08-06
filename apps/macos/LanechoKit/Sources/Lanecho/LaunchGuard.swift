// Startup guard: single instance.
//
// This only guards against launching this app twice. Running both clients on
// one machine (this native build and the Tauri build) is the user's own
// choice: no mutual exclusion, no lock on the data directory — we assume a
// single client at a time.

import AppKit

/// Startup guard
@MainActor
enum LaunchGuard {
    /// Check for a duplicate launch; false means the user was told and this
    /// launch should abort
    ///
    /// The bare `swift run` binary has no bundle id, so it cannot detect its
    /// own duplicates (irrelevant during development).
    static func check() -> Bool {
        guard runningInstancesOfSelf() <= 1 else {
            report(title: L10n.t.guardDuplicateTitle, body: L10n.t.guardDuplicateBody)
            return false
        }
        return true
    }

    /// Number of running instances of this app, including ourselves
    private static func runningInstancesOfSelf() -> Int {
        guard let id = Bundle.main.bundleIdentifier else { return 1 }
        return NSRunningApplication.runningApplications(withBundleIdentifier: id).count
    }

    /// Show an alert and bring the already-running instance to the front
    private static func report(title: String, body: String) {
        if let id = Bundle.main.bundleIdentifier {
            NSRunningApplication.runningApplications(withBundleIdentifier: id)
                .first { $0 != .current }?
                .activate()
        }
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = body
        alert.addButton(withTitle: L10n.t.gotIt)
        alert.runModal()
    }
}
