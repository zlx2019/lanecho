// History panel controller: show/hide, placement and hide on blur. This is the
// window layer; the content lives in HistoryPanelViewController and is built
// from plain AppKit controls.
//
// Toggle race guard: when the menu bar icon is clicked to close the panel,
// mouseDown fires resignKey and auto-hides it first, and only then does the
// button action arrive — without a time window the panel would close and
// immediately reopen. An auto-hide within 300ms counts as "this click has
// already been consumed".

import AppKit
import LanechoKit

/// History panel controller
@MainActor
final class PanelController: NSObject, NSWindowDelegate {
    /// Time window covering the race between hide-on-blur and toggle
    private static let blurHideWindow: Duration = .milliseconds(300)

    private let core: AppCore
    private let panel: FloatingPanel
    private var content: HistoryPanelViewController
    /// Open-preferences callback, forwarded to the content layer and rewired
    /// whenever a language change rebuilds it
    var onOpenSettings: (() -> Void)?
    /// Open-about-window callback
    var onShowAbout: (() -> Void)?
    /// When the panel last auto-hid on blur
    private var lastAutoHide: ContinuousClock.Instant?

    init(core: AppCore) {
        self.core = core
        content = HistoryPanelViewController(core: core)
        panel = FloatingPanel(
            contentRect: NSRect(origin: .zero, size: HistoryPanelViewController.contentSize),
            styleMask: [.nonactivatingPanel, .borderless],
            backing: .buffered, defer: true)
        super.init()

        // Floating shape: high window level, visible on every workspace, and
        // a transparent window body — the rounded vibrancy is drawn by the
        // content's root view
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isFloatingPanel = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.delegate = self
        attach(content)
    }

    /// Attach the content controller and wire it up; shared by the initial
    /// setup and by the language rebuild
    private func attach(_ controller: HistoryPanelViewController) {
        content = controller
        panel.contentViewController = controller
        panel.setContentSize(HistoryPanelViewController.contentSize)
        controller.onHide = { [weak self] in self?.hide() }
        controller.onOpenSettings = { [weak self] in
            self?.hide()
            self?.onOpenSettings?()
        }
        controller.onShowAbout = { [weak self] in
            self?.hide()
            self?.onShowAbout?()
        }
        panel.onDeleteShortcut = { [weak controller] in controller?.deleteSelected() }
        panel.onClearShortcut = { [weak controller] in
            controller?.requestClearFromShortcut()
        }
        panel.onPinShortcut = { [weak controller] in controller?.togglePinSelected() }
        panel.onSettingsShortcut = { [weak self] in
            self?.hide()
            self?.onOpenSettings?()
        }
        controller.onContentHeightChanged = { [weak self] height in
            self?.applyContentHeight(height)
        }
    }

    /// Hug the content the way a menu does: on a height change the **top edge
    /// stays put** and the panel grows downwards, then gets clamped back on
    /// screen
    private func applyContentHeight(_ height: CGFloat) {
        let width = HistoryPanelViewController.panelWidth
        guard abs(panel.frame.height - height) > 0.5 else { return }
        let top = panel.frame.maxY
        var frame = panel.frame
        frame.size = NSSize(width: width, height: height)
        frame.origin.y = top - height
        if let visible = (panel.screen ?? NSScreen.main)?.visibleFrame {
            frame.origin = placePanel(
                anchor: NSPoint(x: frame.minX, y: frame.maxY),
                size: frame.size, visibleFrame: visible)
        }
        panel.setFrame(frame, display: true)
    }

    /// Language switch: AppKit bakes its strings in at construction time, so
    /// rebuilding the whole content controller is the cleanest way
    func relocalize() {
        attach(HistoryPanelViewController(core: core))
    }

    /// Show/hide toggle, driven by a click on the menu bar icon
    func toggle() {
        if panel.isVisible {
            hide()
            return
        }
        // Just auto-hid on blur: this click meant "close", so stay hidden
        if let last = lastAutoHide, ContinuousClock.now - last < Self.blurHideWindow {
            lastAutoHide = nil
            return
        }
        show()
    }

    /// Show on the screen the pointer is on, clamped to its edges; takes key
    /// but never activates the app
    func show() {
        content.prepareForShow()
        let mouse = NSEvent.mouseLocation
        let screen =
            NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main
        // Size from the content we have now; the async list refresh that
        // follows will hug it again
        let size = NSSize(
            width: HistoryPanelViewController.panelWidth, height: content.preferredHeight())
        let visible = screen?.visibleFrame ?? NSRect(origin: .zero, size: size)
        panel.setFrame(
            NSRect(
                origin: placePanel(anchor: mouse, size: size, visibleFrame: visible),
                size: size),
            display: false)
        panel.makeKeyAndOrderFront(nil)
        content.focusSearch()
    }

    /// Hide the panel; the app never activated, so focus falls back to the
    /// original frontmost application. The preview card hides along with it
    func hide() {
        content.panelDidHide()
        panel.orderOut(nil)
    }

    /// History changed: refresh the list while the panel is visible
    func historyChanged() {
        if panel.isVisible {
            content.refreshFromEvent()
        }
    }

    /// Hide on blur
    func windowDidResignKey(_ notification: Notification) {
        if panel.isVisible {
            lastAutoHide = ContinuousClock.now
            hide()
        }
    }
}
