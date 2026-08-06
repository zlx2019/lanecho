// Hotkey coordinator: register / re-register the whole set from settings and
// collect the conflicting ones for the settings page to show (same semantics
// as the Tauri client's get_slot_hotkey_failures).

import Foundation
import LanechoKit

/// Hotkey coordinator
@MainActor
final class HotkeyCoordinator {
    private let center = HotkeyCenter()
    private let core: AppCore
    /// Action that shows or hides the panel
    private let togglePanel: () -> Void
    /// Accelerator strings that failed to register because the system or
    /// another app owns them
    private(set) var failures: [String] = []

    init(core: AppCore, togglePanel: @escaping () -> Void) {
        self.core = core
        self.togglePanel = togglePanel
    }

    /// Re-register the whole set from the current settings: the panel key
    /// plus the Alt+1..6 direct slot pastes
    func reregister(settings: Settings) {
        center.unregisterAll()
        failures = []
        if let spec = parseHotkey(settings.panelHotkey) {
            if !center.register(spec, action: togglePanel) {
                failures.append(settings.panelHotkey)
            }
        }
        if settings.slotHotkeys {
            for slot in 0..<6 {
                let accelerator = "Alt+\(slot + 1)"
                guard let spec = parseHotkey(accelerator) else { continue }
                let core = core
                let registered = center.register(spec) {
                    // Direct slot paste: restore entry n without opening the
                    // panel, and paste too if auto-paste is on
                    Task {
                        guard let id = await core.historyEntryIdAt(slot) else { return }
                        try? await core.restoreEntry(id: id)
                        if await core.currentSettings().autoPaste {
                            AutoPaste.paste()
                        }
                    }
                }
                if !registered {
                    failures.append(accelerator)
                }
            }
        }
    }
}
