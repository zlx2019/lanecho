// Icon assets (the same PNG set the Tauri client uses).
//
// The two build paths resolve resources differently: `swift run` goes through
// SPM's Bundle.module (assets live in the Resources/ subdirectory), while in
// the .app XcodeGen flattens the PNG files into Contents/Resources — try both.

import AppKit

/// Icon entry points (NSImage is not Sendable, so the whole enum is bound to
/// the main thread)
@MainActor
enum Assets {
    /// App icon, used by the about window and system alerts
    static let appIcon: NSImage? = load("AppIcon")

    /// Menu bar icon (a template image, so it inverts with the appearance)
    static let statusItemIcon: NSImage? = {
        guard let image = load("tray-iconTemplate") else { return nil }
        image.isTemplate = true
        // Menu bar icons are sized in points: 18pt (the asset is 88px hi-dpi)
        image.size = NSSize(width: 18, height: 18)
        return image
    }()

    /// Read a PNG out of the resource bundle
    private static func load(_ name: String) -> NSImage? {
        #if SWIFT_PACKAGE
            let bundle = Bundle.module
        #else
            let bundle = Bundle.main
        #endif
        let url =
            bundle.url(forResource: name, withExtension: "png", subdirectory: "Resources")
            ?? bundle.url(forResource: name, withExtension: "png")
        return url.flatMap(NSImage.init(contentsOf:))
    }
}
