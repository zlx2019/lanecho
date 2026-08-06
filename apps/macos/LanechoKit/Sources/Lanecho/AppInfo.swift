// App metadata, shared by the about window and the settings pages.

import Foundation

/// App name
let appName = "lanecho"

/// Version: bundled builds read Info.plist; `swift run` has no bundle and
/// falls back to a development marker
let appVersion: String = {
    guard
        let short = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String,
        !short.isEmpty
    else { return "0.1.0-dev" }
    let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String
    return build.map { "\(short) (\($0))" } ?? short
}()

/// Whether we run inside a real .app bundle (a precondition for system
/// features such as notifications and launch at login)
let isBundled = Bundle.main.bundleIdentifier != nil

/// Project homepage
let appRepositoryURL = URL(string: "https://github.com/zlx2019/lanecho")!

/// Issue tracker
let appIssuesURL = URL(string: "https://github.com/zlx2019/lanecho/issues")!

/// Releases and changelog
let appReleasesURL = URL(string: "https://github.com/zlx2019/lanecho/releases")!

/// Full license text
let appLicenseURL = URL(string: "https://github.com/zlx2019/lanecho/blob/main/LICENSE")!

/// Main open-source dependencies, credited in the about window; keep in sync
/// with Package.swift
let appDependencies = ["SwiftNIO", "swift-crypto", "swift-certificates", "BLAKE3"]
