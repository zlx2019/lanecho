// Compiled ignore-rule evaluator. Built once from IgnoreSettings (regexes
// compiled, file patterns parsed) and rebuilt by the shell when settings
// change; evaluate() then runs on every clipboard change without re-parsing.
//
// Rule semantics (docs/PLAN):
// - Applications and regexes act on text only; file patterns on file lists
//   only; pasteboard types on every kind
// - Regexes must match the WHOLE text (a plain string therefore means an
//   exact match, per the settings page's contract); a pattern that fails to
//   compile degrades to a literal full-string comparison instead of erroring
// - File patterns: one per line, `#` comments and blank lines skipped, no `!`
//   negation. A pattern containing `/` matches against the full path,
//   otherwise against the file name; fnmatch glob syntax (* ? [..]), case
//   insensitive to follow the macOS file system. Any hit ignores the whole
//   batch — filtering the list would silently change what was copied.
// - A rule kind whose both toggles are off contributes nothing; hits from
//   several kinds OR their toggles together

import Foundation

/// The verdict for one clipboard change
public struct IgnoreVerdict: Sendable, Equatable {
    /// Do not broadcast to peers (the LWW baseline still advances)
    public var suppressSync: Bool
    /// Do not record into the local history
    public var suppressRecord: Bool

    /// No rule hit
    public static let none = IgnoreVerdict(suppressSync: false, suppressRecord: false)

    /// Field-wise initializer
    public init(suppressSync: Bool, suppressRecord: Bool) {
        self.suppressSync = suppressSync
        self.suppressRecord = suppressRecord
    }

    /// OR-merge the toggles of one matched rule kind
    fileprivate mutating func merge(sync: Bool, record: Bool) {
        suppressSync = suppressSync || sync
        suppressRecord = suppressRecord || record
    }
}

/// One parsed file pattern
private struct FilePattern: Sendable {
    /// The glob, as written
    var glob: String
    /// A pattern containing "/" matches the full path, otherwise the name
    var againstFullPath: Bool
}

/// The compiled evaluator
///
/// @unchecked Sendable solely for the NSRegularExpression members: the class
/// carries no Sendable annotation, but Apple documents it as immutable and
/// thread-safe, and nothing here mutates after init
public struct IgnoreRules: @unchecked Sendable {
    /// Bundle identifiers of ignored applications
    private let appIds: Set<String>
    /// Ignored pasteboard types
    private let types: Set<String>
    /// Compiled regexes, each wrapped \A(?:...)\z so matching is whole-string
    private let regexes: [NSRegularExpression]
    /// Patterns that failed to compile, kept as literal full-string matches
    private let literals: [String]
    /// Parsed file patterns
    private let filePatterns: [FilePattern]
    /// The toggle pairs (the rest of the settings is not retained)
    private let config: IgnoreSettings

    /// Compile from the settings
    public init(_ settings: IgnoreSettings) {
        appIds = Set(settings.apps.map(\.id))
        types = Set(settings.types)
        var compiled: [NSRegularExpression] = []
        var literals: [String] = []
        for pattern in settings.regexes where !pattern.isEmpty {
            // Anchor the whole pattern; the wrapper group keeps alternations
            // like a|b from escaping the anchors
            if let regex = try? NSRegularExpression(pattern: "\\A(?:\(pattern))\\z") {
                compiled.append(regex)
            } else {
                literals.append(pattern)
            }
        }
        regexes = compiled
        self.literals = literals
        filePatterns = settings.filePatterns.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty && !$0.hasPrefix("#") }
            .map { FilePattern(glob: $0, againstFullPath: $0.contains("/")) }
        config = settings
    }

    /// Evaluate one clipboard change
    ///
    /// - `pasteboardTypes`: the type snapshot the watcher took at read time
    ///   (the pasteboard may have moved on by the time this runs)
    /// - `sourceBundleId`: the frontmost application; pass nil to exempt the
    ///   application rule (restore writes — the content does not come from
    ///   whatever happens to be frontmost)
    public func evaluate(
        content: ClipboardContent, pasteboardTypes: [String], sourceBundleId: String?
    ) -> IgnoreVerdict {
        var verdict = IgnoreVerdict.none
        if !types.isEmpty, pasteboardTypes.contains(where: types.contains) {
            verdict.merge(sync: config.typesSync, record: config.typesRecord)
        }
        switch content {
        case .text(let text):
            if let id = sourceBundleId, appIds.contains(id) {
                verdict.merge(sync: config.appsSync, record: config.appsRecord)
            }
            if matchesText(text) {
                verdict.merge(sync: config.regexSync, record: config.regexRecord)
            }
        case .files(let paths):
            if !filePatterns.isEmpty, paths.contains(where: matchesFile) {
                verdict.merge(sync: config.filesSync, record: config.filesRecord)
            }
        default:
            break
        }
        return verdict
    }

    /// Whether anything can ever match (lets callers skip collecting the
    /// frontmost application when no rule needs it)
    public var wantsSourceApp: Bool {
        !appIds.isEmpty && (config.appsSync || config.appsRecord)
    }

    /// Whole-text regex or literal match
    private func matchesText(_ text: String) -> Bool {
        if literals.contains(text) {
            return true
        }
        guard !regexes.isEmpty else { return false }
        let range = NSRange(text.startIndex..., in: text)
        return regexes.contains { $0.firstMatch(in: text, range: range) != nil }
    }

    /// One path against the pattern list (glob via fnmatch(3); FNM_CASEFOLD
    /// to follow the case-insensitive default of macOS file systems)
    private func matchesFile(_ path: String) -> Bool {
        let name = (path as NSString).lastPathComponent
        return filePatterns.contains { pattern in
            fnmatch(pattern.glob, pattern.againstFullPath ? path : name, FNM_CASEFOLD) == 0
        }
    }
}
