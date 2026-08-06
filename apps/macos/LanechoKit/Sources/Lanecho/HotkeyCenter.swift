// Global hotkey registry, built on Carbon RegisterEventHotKey — what
// libraries like KeyboardShortcuts sit on anyway, so use the system API
// directly and skip the dependency.
//
// One application-level event handler dispatching by id. On a conflict
// (another app already owns the combination) register returns false and the
// layer above can surface it in settings (same semantics as the Tauri
// client's get_slot_hotkey_failures).

import AppKit
import Carbon
import LanechoKit

/// Global hotkey registry
@MainActor
final class HotkeyCenter {
    /// Registered entries: id → (system handle, action)
    private var hotkeys: [UInt32: (ref: EventHotKeyRef, action: () -> Void)] = [:]
    /// Handle of the application-level event handler
    private var handlerRef: EventHandlerRef?
    /// id allocator
    private var nextId: UInt32 = 1
    /// Signature (FourCharCode "lnec")
    private static let signature: OSType = 0x6C6E_6563

    init() {
        // Install the one application-level kEventHotKeyPressed handler
        var eventType = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        let userData = Unmanaged.passUnretained(self).toOpaque()
        InstallEventHandler(
            GetApplicationEventTarget(),
            { _, event, userData in
                guard let event, let userData else { return noErr }
                var hotkeyId = EventHotKeyID()
                GetEventParameter(
                    event, EventParamName(kEventParamDirectObject),
                    EventParamType(typeEventHotKeyID), nil,
                    MemoryLayout<EventHotKeyID>.size, nil, &hotkeyId)
                let center = Unmanaged<HotkeyCenter>.fromOpaque(userData)
                    .takeUnretainedValue()
                // The Carbon callback lands on the main thread (application
                // event target), so dispatch directly
                MainActor.assumeIsolated {
                    center.dispatch(id: hotkeyId.id)
                }
                return noErr
            }, 1, &eventType, userData, &handlerRef)
    }

    /// Register one global hotkey; returns false when the system or another
    /// app already owns it
    @discardableResult
    func register(_ spec: HotkeySpec, action: @escaping () -> Void) -> Bool {
        let id = nextId
        nextId += 1
        var ref: EventHotKeyRef?
        let status = RegisterEventHotKey(
            spec.keyCode, spec.modifiers,
            EventHotKeyID(signature: Self.signature, id: id),
            GetApplicationEventTarget(), 0, &ref)
        guard status == noErr, let ref else { return false }
        hotkeys[id] = (ref, action)
        return true
    }

    /// Unregister everything, ahead of a bulk re-registration after a
    /// settings change
    func unregisterAll() {
        for (_, entry) in hotkeys {
            UnregisterEventHotKey(entry.ref)
        }
        hotkeys.removeAll()
    }

    /// Dispatch the action registered under an id
    private func dispatch(id: UInt32) {
        hotkeys[id]?.action()
    }
}
