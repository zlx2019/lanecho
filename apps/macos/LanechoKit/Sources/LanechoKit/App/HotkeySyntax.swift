// Tauri hotkey syntax parsing (pure functions; settings.json's panelHotkey is
// the single source of truth both clients read, so the native client keeps no
// storage of its own — it parses the string into a Carbon key code and
// registers that locally)
//
// Supported modifier tokens (case-insensitive): CmdOrCtrl/CommandOrControl
// (⌘ on macOS), Cmd/Command/Super, Ctrl/Control, Alt/Option, Shift.
// Main keys: A-Z, 0-9, F1-F12, Space, arrow keys and common punctuation.

import Foundation

/// Parse result: the key code and modifier bits Carbon's RegisterEventHotKey
/// needs
public struct HotkeySpec: Sendable, Equatable {
    /// Carbon virtual key code (ANSI layout)
    public var keyCode: UInt32
    /// Carbon modifier bits (combination of cmdKey/shiftKey/optionKey/controlKey)
    public var modifiers: UInt32
}

/// Carbon modifier bit constants (so the shell layer need not import Carbon
/// just to assemble the bits)
public enum CarbonModifier {
    public static let command: UInt32 = 0x0100
    public static let shift: UInt32 = 0x0200
    public static let option: UInt32 = 0x0800
    public static let control: UInt32 = 0x1000
}

/// Parses a Tauri accelerator string; returns nil for an empty string, an
/// unknown token, or a missing main key
public func parseHotkey(_ accelerator: String) -> HotkeySpec? {
    var modifiers: UInt32 = 0
    var keyCode: UInt32?
    for rawToken in accelerator.split(separator: "+") {
        let token = rawToken.trimmingCharacters(in: .whitespaces).lowercased()
        switch token {
        case "cmdorctrl", "commandorcontrol", "cmd", "command", "super", "meta":
            modifiers |= CarbonModifier.command
        case "ctrl", "control":
            modifiers |= CarbonModifier.control
        case "alt", "option":
            modifiers |= CarbonModifier.option
        case "shift":
            modifiers |= CarbonModifier.shift
        default:
            // Only one main key is allowed
            guard keyCode == nil, let code = carbonKeyCode(of: token) else { return nil }
            keyCode = code
        }
    }
    guard let keyCode else { return nil }
    return HotkeySpec(keyCode: keyCode, modifiers: modifiers)
}

/// Reverse serialization: Carbon key code plus modifier bits → Tauri
/// accelerator string (written back to settings.json once the settings page
/// records a new binding; ⌘ serializes as CmdOrCtrl to keep the cross-platform
/// semantics of the Tauri client). Returns nil with no modifier or an unknown
/// key code.
public func hotkeyAccelerator(keyCode: UInt32, modifiers: UInt32) -> String? {
    guard modifiers != 0, let key = tokenName(of: keyCode) else { return nil }
    var parts: [String] = []
    if modifiers & CarbonModifier.command != 0 { parts.append("CmdOrCtrl") }
    if modifiers & CarbonModifier.control != 0 { parts.append("Ctrl") }
    if modifiers & CarbonModifier.option != 0 { parts.append("Alt") }
    if modifiers & CarbonModifier.shift != 0 { parts.append("Shift") }
    guard !parts.isEmpty else { return nil }
    parts.append(key)
    return parts.joined(separator: "+")
}

/// Display symbol for a slot modifier token ("CmdOrCtrl" → ⌘ on this
/// platform); unknown values fall back to ⌘, mirroring the settings
/// normalization. Used by the panel slot hints and the settings page
public func slotModifierSymbol(_ modifier: String) -> String {
    switch modifier {
    case "Alt": "⌥"
    case "Ctrl": "⌃"
    default: "⌘"
    }
}

/// Key code → canonical token (single-character keys uppercased, function keys
/// keep their conventional spelling)
private func tokenName(of keyCode: UInt32) -> String? {
    guard let token = keyTokenTable.first(where: { $0.value == keyCode })?.key else {
        return nil
    }
    return token.count == 1 ? token.uppercased() : token.capitalized
}

/// Main-key token → Carbon ANSI virtual key code
private func carbonKeyCode(of token: String) -> UInt32? {
    keyTokenTable[token]
}

/// Table shared by both directions: token ↔ key code
private let keyTokenTable: [String: UInt32] = {
    let table: [String: UInt32] = [
        "a": 0x00, "s": 0x01, "d": 0x02, "f": 0x03, "h": 0x04, "g": 0x05,
        "z": 0x06, "x": 0x07, "c": 0x08, "v": 0x09, "b": 0x0B, "q": 0x0C,
        "w": 0x0D, "e": 0x0E, "r": 0x0F, "y": 0x10, "t": 0x11,
        "1": 0x12, "2": 0x13, "3": 0x14, "4": 0x15, "6": 0x16, "5": 0x17,
        "=": 0x18, "9": 0x19, "7": 0x1A, "-": 0x1B, "8": 0x1C, "0": 0x1D,
        "]": 0x1E, "o": 0x1F, "u": 0x20, "[": 0x21, "i": 0x22, "p": 0x23,
        "l": 0x25, "j": 0x26, "'": 0x27, "k": 0x28, ";": 0x29, "\\": 0x2A,
        ",": 0x2B, "/": 0x2C, "n": 0x2D, "m": 0x2E, ".": 0x2F, "`": 0x32,
        "space": 0x31, "tab": 0x30, "enter": 0x24, "return": 0x24,
        "backspace": 0x33, "delete": 0x75, "escape": 0x35, "esc": 0x35,
        "home": 0x73, "end": 0x77, "pageup": 0x74, "pagedown": 0x79,
        "left": 0x7B, "right": 0x7C, "down": 0x7D, "up": 0x7E,
        "f1": 0x7A, "f2": 0x78, "f3": 0x63, "f4": 0x76, "f5": 0x60,
        "f6": 0x61, "f7": 0x62, "f8": 0x64, "f9": 0x65, "f10": 0x6D,
        "f11": 0x67, "f12": 0x6F,
    ]
    return table
}()
