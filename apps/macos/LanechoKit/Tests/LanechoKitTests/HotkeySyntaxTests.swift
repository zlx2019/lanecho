// Tests for parsing the Tauri hotkey syntax

import Testing

@testable import LanechoKit

/// Default panel hotkey: CmdOrCtrl resolves to ⌘ on macOS
@Test func hotkeyParsesDefaultPanelKey() {
    let spec = parseHotkey("CmdOrCtrl+Shift+V")
    #expect(
        spec
            == HotkeySpec(
                keyCode: 0x09,
                modifiers: CarbonModifier.command | CarbonModifier.shift))
}

/// Slot keys Alt+1..6, and tokens match case-insensitively
@Test func hotkeyParsesSlotKeysAndCase() {
    #expect(parseHotkey("Alt+1") == HotkeySpec(keyCode: 0x12, modifiers: CarbonModifier.option))
    #expect(parseHotkey("alt+6") == HotkeySpec(keyCode: 0x16, modifiers: CarbonModifier.option))
    #expect(parseHotkey("OPTION+SHIFT+F5")?.keyCode == 0x60)
}

/// Every modifier family, plus function keys
@Test func hotkeyParsesModifierFamilies() {
    let spec = parseHotkey("Ctrl+Alt+Shift+Space")
    #expect(
        spec?.modifiers
            == CarbonModifier.control | CarbonModifier.option | CarbonModifier.shift)
    #expect(spec?.keyCode == 0x31)
    #expect(parseHotkey("Super+Enter")?.modifiers == CarbonModifier.command)
}

/// Invalid input: empty string / modifiers only / unknown token / two main keys
@Test func hotkeyRejectsInvalidInput() {
    #expect(parseHotkey("") == nil)
    #expect(parseHotkey("Cmd+Shift") == nil)
    #expect(parseHotkey("Cmd+Hyper+V") == nil)
    #expect(parseHotkey("Cmd+A+B") == nil)
}

/// Reverse serialization: a recorded hotkey is written back as Tauri syntax and
/// round-trips through the parser
@Test func hotkeySerializerRoundtrips() throws {
    let text = try #require(
        hotkeyAccelerator(
            keyCode: 0x09, modifiers: CarbonModifier.command | CarbonModifier.shift))
    #expect(text == "CmdOrCtrl+Shift+V")
    #expect(parseHotkey(text) == HotkeySpec(keyCode: 0x09, modifiers: 0x0300))

    #expect(
        hotkeyAccelerator(keyCode: 0x60, modifiers: CarbonModifier.option) == "Alt+F5")
    // Reject an unmodified key (a bare global hotkey would swallow normal
    // typing) and an unknown key code
    #expect(hotkeyAccelerator(keyCode: 0x09, modifiers: 0) == nil)
    #expect(hotkeyAccelerator(keyCode: 0xFFFF, modifiers: CarbonModifier.command) == nil)
}
