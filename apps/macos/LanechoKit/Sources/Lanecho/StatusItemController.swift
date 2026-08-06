// Menu bar icon: both left and right click toggle the panel. What used to be
// menu items now lives in the panel footer; macOS gets no native tray menu.

import AppKit

/// Menu bar icon controller
@MainActor
final class StatusItemController {
    /// The status bar item, retained so it is not reclaimed
    private let item: NSStatusItem
    /// Click callback
    private let toggle: () -> Void

    init(toggle: @escaping () -> Void) {
        self.toggle = toggle
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = item.button {
            button.image =
                Assets.statusItemIcon
                ?? NSImage(
                    systemSymbolName: "doc.on.clipboard", accessibilityDescription: "lanecho")
            button.target = self
            button.action = #selector(clicked)
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            button.toolTip = appName
        }
    }

    /// Refresh the tooltip from the pending pairing request count
    ///
    /// An accessory app has no Dock badge, and system notifications are
    /// transient — the menu bar tooltip is the only persistent cue left once
    /// a notification is missed (same semantics as the Tauri client's
    /// update_pending_tooltip)
    func updateTooltip(pendingPairs: Int) {
        item.button?.toolTip = pendingPairs > 0 ? L10n.t.trayPending(pendingPairs) : appName
    }

    @objc private func clicked() {
        toggle()
    }
}
