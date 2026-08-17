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
    ///
    /// Command-carrying shortcuts only: AppKit routes a key-down into the
    /// key-equivalent chain only when it holds Command or Control, so an
    /// Option-only shortcut placed here is dead code — ⌥P lives in
    /// sendEvent below.
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

    /// ⌥P: an Option-only key-down never reaches performKeyEquivalent
    /// (verified by feeding synthesized events through NSApp.sendEvent), so
    /// it must be caught on sendEvent — the path every key-down does take —
    /// before the field editor types it into the search field as π.
    /// charactersIgnoringModifiers still yields the base key under Option;
    /// repeats are dropped so holding the key does not flap the pin state.
    override func sendEvent(_ event: NSEvent) {
        if event.type == .keyDown, !event.isARepeat,
            event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .option,
            event.charactersIgnoringModifiers?.lowercased() == "p"
        {
            onPinShortcut?()
            return
        }
        super.sendEvent(event)
    }
}
