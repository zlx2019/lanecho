// Ignore rules configuration, stored as the `ignore` object inside
// settings.json (shared with the Tauri client under the same keys — its
// Settings struct carries the same shape so a save from either side keeps the
// other's values).
//
// Four rule kinds: source applications (text only), pasteboard types, regular
// expressions (text only) and file patterns. Each kind carries its own pair of
// toggles — suppress sync / suppress history recording — defaulting to sync
// suppressed, recording kept.
//
// The concealed-marker hard stop in the watcher is NOT driven by this list:
// content flagged org.nspasteboard.ConcealedType is never read, recorded or
// synced regardless of what the user edits here. The default type list still
// names the well-known transient/concealed types so the extra representations
// some password managers post are covered too.

import Foundation

/// One ignored source application: matching runs on the bundle identifier
/// (stable across localizations); the name is only what the settings page
/// shows, frozen at add time
public struct IgnoredApp: Codable, Sendable, Equatable {
    /// Bundle identifier ("com.google.Chrome")
    public var id: String
    /// Display name at the time it was added
    public var name: String

    /// Field-wise initializer
    public init(id: String, name: String) {
        self.id = id
        self.name = name
    }
}

/// The ignore configuration (an object under the `ignore` key)
public struct IgnoreSettings: Codable, Sendable, Equatable {
    /// Applications whose **text** copies are ignored
    public var apps: [IgnoredApp]
    /// Pasteboard types that mark a change as ignored
    public var types: [String]
    /// Regular expressions searched against the text as written (anchor with
    /// ^ $ for an exact match); a pattern that fails to compile degrades to a
    /// literal substring check
    public var regexes: [String]
    /// File ignore rules, one pattern per line in a simplified .gitignore
    /// syntax (see IgnoreRules); stored as the raw editor text
    public var filePatterns: String
    /// Application rule: suppress sync / suppress recording
    public var appsSync: Bool
    /// Application rule: suppress history recording
    public var appsRecord: Bool
    /// Type rule: suppress sync
    public var typesSync: Bool
    /// Type rule: suppress history recording
    public var typesRecord: Bool
    /// Regex rule: suppress sync
    public var regexSync: Bool
    /// Regex rule: suppress history recording
    public var regexRecord: Bool
    /// File rule: suppress sync
    public var filesSync: Bool
    /// File rule: suppress history recording
    public var filesRecord: Bool

    /// The default pasteboard type list (the well-known transient/concealed
    /// conventions plus common password managers); the settings page's reset
    /// button restores exactly this list
    public static let defaultTypes: [String] = [
        "de.petermaurer.TransientPasteboardType",
        "org.nspasteboard.TransientType",
        "org.nspasteboard.ConcealedType",
        "com.agilebits.onepassword",
        "net.antelle.keeweb",
        "com.typeit4me.clipping",
    ]

    /// Defaults: no user entries, the well-known type list, every rule kind
    /// suppressing sync but keeping history recording
    public init(
        apps: [IgnoredApp] = [], types: [String] = IgnoreSettings.defaultTypes,
        regexes: [String] = [], filePatterns: String = "",
        appsSync: Bool = true, appsRecord: Bool = false,
        typesSync: Bool = true, typesRecord: Bool = false,
        regexSync: Bool = true, regexRecord: Bool = false,
        filesSync: Bool = true, filesRecord: Bool = false
    ) {
        self.apps = apps
        self.types = types
        self.regexes = regexes
        self.filePatterns = filePatterns
        self.appsSync = appsSync
        self.appsRecord = appsRecord
        self.typesSync = typesSync
        self.typesRecord = typesRecord
        self.regexSync = regexSync
        self.regexRecord = regexRecord
        self.filesSync = filesSync
        self.filesRecord = filesRecord
    }

    /// Lenient decoding: any missing field takes its default (serde default
    /// semantics, matching the Settings decoder)
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let defaults = IgnoreSettings()
        apps = try container.decodeIfPresent([IgnoredApp].self, forKey: .apps) ?? defaults.apps
        types = try container.decodeIfPresent([String].self, forKey: .types) ?? defaults.types
        regexes =
            try container.decodeIfPresent([String].self, forKey: .regexes) ?? defaults.regexes
        filePatterns =
            try container.decodeIfPresent(String.self, forKey: .filePatterns)
            ?? defaults.filePatterns
        appsSync = try container.decodeIfPresent(Bool.self, forKey: .appsSync) ?? defaults.appsSync
        appsRecord =
            try container.decodeIfPresent(Bool.self, forKey: .appsRecord) ?? defaults.appsRecord
        typesSync =
            try container.decodeIfPresent(Bool.self, forKey: .typesSync) ?? defaults.typesSync
        typesRecord =
            try container.decodeIfPresent(Bool.self, forKey: .typesRecord) ?? defaults.typesRecord
        regexSync =
            try container.decodeIfPresent(Bool.self, forKey: .regexSync) ?? defaults.regexSync
        regexRecord =
            try container.decodeIfPresent(Bool.self, forKey: .regexRecord) ?? defaults.regexRecord
        filesSync =
            try container.decodeIfPresent(Bool.self, forKey: .filesSync) ?? defaults.filesSync
        filesRecord =
            try container.decodeIfPresent(Bool.self, forKey: .filesRecord) ?? defaults.filesRecord
    }
}
