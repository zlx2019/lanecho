// Non-activating floating panel, the combination Maccy already proved out.
//
// The point: the panel can become the key window so the search field takes
// typing, yet the app **never activates** — the frontmost application stays
// frontmost, focus is still where it was once the panel hides, and ⌘V just
// works. This is the foundation of the whole history panel interaction;
// canBecomeKey cannot be dropped.

import AppKit

/// Non-activating floating panel
final class FloatingPanel: NSPanel {
    /// ⌘⌫ deletes the highlighted entry
    var onDeleteShortcut: (() -> Void)?
    /// ⌥⌘⌫ clears the history
    var onClearShortcut: (() -> Void)?
    /// ⌘, opens the preferences
    var onSettingsShortcut: (() -> Void)?
    /// ⌥P pins or unpins the highlighted entry
    var onPinShortcut: (() -> Void)?

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    /// Intercept on the key-equivalent chain, which must come before the
    /// search field's field editor — that would eat ⌘⌫ as "delete to start of
    /// line". The app has no menu bar, so even ⌘Q has to be handled here.
    /// Every shortcut advertised must actually be implemented; the footer
    /// never hints at one that is not.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        // keyCode 51 = ⌫
        if event.keyCode == 51 {
            if modifiers == [.command, .option] {
                onClearShortcut?()
                return true
            }
            if modifiers == .command {
                onDeleteShortcut?()
                return true
            }
        }
        // ⌥P: the field editor would type it as π, so it has to be caught at
        // this layer; charactersIgnoringModifiers still yields the base key
        // when Option is held
        if modifiers == .option, event.charactersIgnoringModifiers?.lowercased() == "p" {
            onPinShortcut?()
            return true
        }
        if modifiers == .command, let key = event.charactersIgnoringModifiers {
            switch key {
            case ",":
                onSettingsShortcut?()
                return true
            case "q":
                NSApp.terminate(nil)
                return true
            default:
                break
            }
        }
        return super.performKeyEquivalent(with: event)
    }
}
