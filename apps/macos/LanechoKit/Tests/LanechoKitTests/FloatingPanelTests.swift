// Regression tests for which AppKit dispatch layer each panel shortcut hooks.
//
// AppKit only routes key-downs carrying Command or Control into the
// key-equivalent chain; an Option-only press goes straight to
// window.sendEvent and on to the field editor. ⌥P used to sit in
// performKeyEquivalent — dead code: the pin shortcut did nothing on a real
// machine while every ⌘-based shortcut worked. These tests pin ⌥P to the
// sendEvent layer so it cannot drift back.
import AppKit
import XCTest

@testable import Lanecho

@MainActor
final class FloatingPanelTests: XCTestCase {
    private func makePanel() -> FloatingPanel {
        FloatingPanel(
            contentRect: NSRect(x: 0, y: 0, width: 320, height: 200),
            styleMask: [.nonactivatingPanel, .titled],
            backing: .buffered,
            defer: false
        )
    }

    private func key(
        _ type: NSEvent.EventType, modifiers: NSEvent.ModifierFlags,
        characters: String, base: String, keyCode: UInt16, isARepeat: Bool = false
    ) -> NSEvent {
        NSEvent.keyEvent(
            with: type, location: .zero, modifierFlags: modifiers,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: 0,
            context: nil, characters: characters, charactersIgnoringModifiers: base,
            isARepeat: isARepeat, keyCode: keyCode
        )!
    }

    /// keyCode 35 = P; with Option held the characters read π but the base key stays p
    private func optionP(isARepeat: Bool = false, type: NSEvent.EventType = .keyDown) -> NSEvent {
        key(type, modifiers: .option, characters: "π", base: "p", keyCode: 35, isARepeat: isARepeat)
    }

    func testOptionPFiresThePinShortcutOnSendEvent() {
        let panel = makePanel()
        var fired = 0
        panel.onPinShortcut = { fired += 1 }
        panel.sendEvent(optionP())
        XCTAssertEqual(fired, 1, "⌥P must be caught on sendEvent — performKeyEquivalent never sees it")
    }

    func testOptionPIgnoresRepeatsAndKeyUps() {
        let panel = makePanel()
        var fired = 0
        panel.onPinShortcut = { fired += 1 }
        panel.sendEvent(optionP(isARepeat: true))
        panel.sendEvent(optionP(type: .keyUp))
        XCTAssertEqual(fired, 0, "holding ⌥P must not flap the pin state; keyUp must not double-fire")
    }

    func testOtherModifierCombinationsDoNotPin() {
        let panel = makePanel()
        var fired = 0
        panel.onPinShortcut = { fired += 1 }
        panel.sendEvent(key(.keyDown, modifiers: [.option, .shift], characters: "∏", base: "P", keyCode: 35))
        panel.sendEvent(key(.keyDown, modifiers: [.option, .command], characters: "π", base: "p", keyCode: 35))
        XCTAssertEqual(fired, 0)
    }

    func testCommandShortcutsStayOnTheKeyEquivalentChain() {
        let panel = makePanel()
        var deleted = 0
        var cleared = 0
        panel.onDeleteShortcut = { deleted += 1 }
        panel.onClearShortcut = { cleared += 1 }
        // keyCode 51 = ⌫; these do arrive via performKeyEquivalent for real —
        // AppKit dispatches Command-carrying key-downs into that chain
        XCTAssertTrue(
            panel.performKeyEquivalent(
                with: key(.keyDown, modifiers: .command, characters: "\u{7f}", base: "\u{7f}", keyCode: 51)))
        XCTAssertTrue(
            panel.performKeyEquivalent(
                with: key(
                    .keyDown, modifiers: [.command, .option], characters: "\u{7f}", base: "\u{7f}", keyCode: 51)))
        XCTAssertEqual(deleted, 1)
        XCTAssertEqual(cleared, 1)
    }
}
