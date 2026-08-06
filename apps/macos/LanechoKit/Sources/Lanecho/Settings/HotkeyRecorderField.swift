// Hotkey recorder control (a Maccy-style pill): a rounded box showing the
// hotkey symbols centred, with an ⓧ on the right to clear it. Clicking the box
// enters recording mode (highlighted border plus a "press keys" prompt) and a
// local event monitor captures the next modified keystroke, serialized back
// into the Tauri accelerator syntax so it round-trips through the parser.

import AppKit
import LanechoKit
import SwiftUI

/// Hotkey recorder pill
struct HotkeyRecorderField: View {
    /// Current accelerator string (empty = unbound)
    let current: String
    /// Text table
    let texts: Texts
    /// Callback for a recorded or cleared hotkey
    let onChange: (String) -> Void

    @State private var recording = false
    @State private var monitor: Any?

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 6)
                .fill(Color(nsColor: .textBackgroundColor))
            RoundedRectangle(cornerRadius: 6)
                .stroke(
                    recording ? Color.accentColor : Color.primary.opacity(0.18),
                    lineWidth: recording ? 1.5 : 1)
            Text(displayText)
                .font(.system(size: 12, weight: current.isEmpty ? .regular : .medium))
                .foregroundStyle(current.isEmpty && !recording ? .secondary : .primary)
                .padding(.horizontal, 20)
                .lineLimit(1)
            if !current.isEmpty, !recording {
                HStack {
                    Spacer()
                    Button {
                        onChange("")
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 12))
                            .foregroundStyle(.tertiary)
                    }
                    .buttonStyle(.plain)
                    .padding(.trailing, 5)
                }
            }
        }
        .frame(width: 150, height: 22)
        .contentShape(Rectangle())
        .onTapGesture {
            recording ? stopRecording() : startRecording()
        }
        .onDisappear {
            stopRecording()
        }
    }

    /// Text inside the pill: recording > bound symbols > unbound prompt
    private var displayText: String {
        if recording {
            return texts.hotkeyPressKeys
        }
        return current.isEmpty ? texts.hotkeyRecord : Self.symbolize(current)
    }

    /// Start recording: the local monitor swallows keystrokes; Esc cancels, a
    /// modified key commits
    private func startRecording() {
        recording = true
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            handle(event)
            return nil
        }
    }

    private func stopRecording() {
        recording = false
        if let monitor {
            NSEvent.removeMonitor(monitor)
        }
        monitor = nil
    }

    private func handle(_ event: NSEvent) {
        let modifiers = Self.carbonModifiers(of: event.modifierFlags)
        // Bare Esc cancels recording
        if event.keyCode == 53, modifiers == 0 {
            stopRecording()
            return
        }
        // A bare key with no modifier cannot be a global hotkey (it would
        // swallow normal typing), so keep waiting
        guard
            let accelerator = hotkeyAccelerator(
                keyCode: UInt32(event.keyCode), modifiers: modifiers)
        else { return }
        stopRecording()
        onChange(accelerator)
    }

    /// NSEvent modifier bits → Carbon modifier bits
    private static func carbonModifiers(of flags: NSEvent.ModifierFlags) -> UInt32 {
        var modifiers: UInt32 = 0
        if flags.contains(.command) { modifiers |= CarbonModifier.command }
        if flags.contains(.control) { modifiers |= CarbonModifier.control }
        if flags.contains(.option) { modifiers |= CarbonModifier.option }
        if flags.contains(.shift) { modifiers |= CarbonModifier.shift }
        return modifiers
    }

    /// Accelerator string → symbol display (CmdOrCtrl+Shift+V → ⇧⌘V, modifiers
    /// in the order the system uses)
    static func symbolize(_ accelerator: String) -> String {
        var control = false
        var option = false
        var shift = false
        var command = false
        var key = ""
        for token in accelerator.split(separator: "+") {
            switch token.lowercased() {
            case "cmdorctrl", "commandorcontrol", "cmd", "command", "super", "meta":
                command = true
            case "ctrl", "control": control = true
            case "alt", "option": option = true
            case "shift": shift = true
            default: key = token.count == 1 ? token.uppercased() : String(token)
            }
        }
        // The order the system uses: ⌃⌥⇧⌘
        var parts: [String] = []
        if control { parts.append("⌃") }
        if option { parts.append("⌥") }
        if shift { parts.append("⇧") }
        if command { parts.append("⌘") }
        parts.append(key)
        return parts.joined()
    }
}
