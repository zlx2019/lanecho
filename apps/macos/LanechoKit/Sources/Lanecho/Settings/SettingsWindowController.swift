// Settings window controller: the classic preferences window pattern, as in
// Maccy and Easydict.
//
// - Tabs = NSTabViewController(.toolbar) + window.toolbarStyle = .preference:
//   the toolbar icons sit in the window chrome and the window title follows the
//   selected tab
// - **The window size is fixed once, when the window opens**: the tallest of
//   the five pages wins (page content is non-scrolling cards with a real
//   intrinsic height) and switching tabs never resizes it — a jump on every
//   switch looks terrible. It only ever **grows, never shrinks**, and only when
//   dynamic content (the hotkey conflict list) overflows
// - Clear the first responder on open and after each tab switch — AppKit
//   focuses the first text field automatically, and opening settings straight
//   into a lit-up device name field is jarring

import AppKit
import LanechoKit
import SwiftUI

/// Settings window controller
@MainActor
final class SettingsWindowController: NSObject {
    private let model: SettingsModel
    private var window: NSWindow?
    private var tabs: SettingsTabViewController?

    init(model: SettingsModel) {
        self.model = model
    }

    /// Builds the settings window without showing it.
    ///
    /// Creating all five SwiftUI tabs and measuring their fitting sizes is
    /// synchronous main-thread work. The app calls this once after startup so
    /// the first settings click only has to reveal the already prepared
    /// window. `show()` also calls it as a fallback when the user gets there
    /// before the deferred warm-up runs.
    func prepare() {
        guard window == nil else { return }
        let tabs = SettingsTabViewController()
        tabs.tabStyle = .toolbar
        for definition in Self.tabDefinitions(model: model) {
            tabs.addTabViewItem(Self.makeTab(definition))
        }
        let window = NSWindow(contentViewController: tabs)
        window.styleMask = [.titled, .closable, .miniaturizable]
        window.toolbarStyle = .preference
        window.isReleasedWhenClosed = false
        // Size fixed once: the tallest page decides the window height, the
        // rest get slack at the bottom
        window.setContentSize(
            NSSize(width: settingsPageWidth, height: tabs.tallestPageHeight()))
        window.center()
        self.tabs = tabs
        self.window = window
        // Grow (never shrink) when dynamic content such as the hotkey
        // conflict list overflows the window
        model.onContentSizeChanged = { [weak tabs] in
            tabs?.growWindowIfContentOverflows()
        }
    }

    /// Show the prepared settings window; activates the app and brings it to
    /// the front
    func show() {
        prepare()
        Task { await model.load() }
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
        window?.makeFirstResponder(nil)
    }

    /// Language switch: update the tab labels and titles in place (the contents
    /// are SwiftUI reading model.texts and update themselves)
    func relocalize() {
        guard let tabs else { return }
        for (item, definition) in zip(tabs.tabViewItems, Self.tabDefinitions(model: model)) {
            item.label = definition.title
            item.viewController?.title = definition.title
        }
        if let selected = tabs.tabView.selectedTabViewItem {
            window?.title = selected.label
        }
    }

    /// Tab definitions (the order is the usage flow)
    private static func tabDefinitions(
        model: SettingsModel
    ) -> [(title: String, symbol: String, view: AnyView)] {
        let texts = L10n.t
        return [
            (texts.tabGeneral, "gearshape", AnyView(GeneralTab(model: model))),
            (texts.tabDevices, "wifi", AnyView(DevicesTab(model: model))),
            (texts.tabSync, "arrow.triangle.2.circlepath", AnyView(SyncTab(model: model))),
            (texts.tabStorage, "internaldrive", AnyView(StorageTab(model: model))),
            (texts.tabHotkeys, "keyboard", AnyView(HotkeysTab(model: model))),
        ]
    }

    /// Assemble one tab
    private static func makeTab(
        _ definition: (title: String, symbol: String, view: AnyView)
    ) -> NSTabViewItem {
        let hosting = NSHostingController(rootView: definition.view)
        hosting.title = definition.title
        let item = NSTabViewItem(viewController: hosting)
        item.label = definition.title
        item.image = NSImage(
            systemSymbolName: definition.symbol, accessibilityDescription: definition.title)
        return item
    }
}

/// Tab container: fixed size, a tab switch only clears focus and leaves the
/// window alone
private final class SettingsTabViewController: NSTabViewController {
    override func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        super.tabView(tabView, didSelect: tabViewItem)
        view.window?.makeFirstResponder(nil)
    }

    /// Content height of the tallest page (touching view triggers loadView on
    /// every page, so this is called once while building the window)
    func tallestPageHeight() -> CGFloat {
        let heights = tabViewItems.compactMap { $0.viewController?.view.fittingSize.height }
        return heights.max() ?? 420
    }

    /// Grow when dynamic content overflows the window (grow only, top edge
    /// stays put)
    func growWindowIfContentOverflows() {
        guard let window = view.window else { return }
        let needed = tallestPageHeight()
        let current = window.contentRect(forFrameRect: window.frame).height
        guard needed > current else { return }
        let target = window.frameRect(
            forContentRect: NSRect(x: 0, y: 0, width: settingsPageWidth, height: needed))
        var frame = window.frame
        frame.origin.y += frame.height - target.height
        frame.size = target.size
        window.setFrame(frame, display: true, animate: true)
    }
}
