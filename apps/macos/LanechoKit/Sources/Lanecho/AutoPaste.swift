// Auto-paste on selection (a native-only extra, off by default).
//
// Once the panel hides, synthesize one ⌘V into the frontmost application.
// This requires **Accessibility permission**: without it the system silently
// drops CGEvent.post (no error, no effect), so flipping the toggle on must
// check and onboard right then, or the user just sees a switch that does
// nothing.
//
// Timing: the key must not go out until the panel has actually hidden and
// focus is back with the original app. The panel is a non-activating window,
// so the frontmost app never changed, but keyboard focus sits on the panel
// and an early keystroke gets eaten by it.

import AppKit
import ApplicationServices

/// Auto-paste
@MainActor
enum AutoPaste {
    /// Gap between hiding the panel and sending the key, leaving a beat for
    /// the window to hide and focus to settle
    private static let delay: Duration = .milliseconds(80)
    /// ANSI virtual key code for V
    private static let keyCodeV: CGKeyCode = 0x09

    /// Whether Accessibility permission is currently granted
    static var isTrusted: Bool {
        AXIsProcessTrusted()
    }

    /// Request permission (true = show the system authorization prompt); the
    /// grant only takes effect after a restart, which the caller must say
    ///
    /// The key is spelled out as a literal: `kAXTrustedCheckOptionPrompt` is a
    /// mutable global, which Swift 6 strict concurrency refuses to reference
    @discardableResult
    static func requestTrust(prompt: Bool) -> Bool {
        let options = ["AXTrustedCheckOptionPrompt": prompt] as CFDictionary
        return AXIsProcessTrustedWithOptions(options)
    }

    /// Open the Accessibility pane in System Settings
    static func openAccessibilitySettings() {
        guard
            let url = URL(
                string:
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            )
        else { return }
        NSWorkspace.shared.open(url)
    }

    /// Synthesize one ⌘V into the frontmost application; a no-op without
    /// permission
    static func paste() {
        guard isTrusted else { return }
        Task {
            try? await Task.sleep(for: delay)
            postCommandV()
        }
    }

    /// Synthesize the keystroke on a session-level event source, the same
    /// path a real keyboard takes
    private static func postCommandV() {
        guard let source = CGEventSource(stateID: .combinedSessionState) else { return }
        let down = CGEvent(keyboardEventSource: source, virtualKey: keyCodeV, keyDown: true)
        let up = CGEvent(keyboardEventSource: source, virtualKey: keyCodeV, keyDown: false)
        down?.flags = .maskCommand
        up?.flags = .maskCommand
        down?.post(tap: .cghidEventTap)
        up?.post(tap: .cghidEventTap)
    }
}
