// Ignore-rule evaluator tests (pure functions; no clipboard, no engine)

import Foundation
import Testing

@testable import LanechoKit

/// Convenience: rules over a settings value patched by the caller
private func rules(_ patch: (inout IgnoreSettings) -> Void) -> IgnoreRules {
    var settings = IgnoreSettings()
    patch(&settings)
    return IgnoreRules(settings)
}

/// Application rule: bundle-id hit on text only; nil exempts (restore writes);
/// other content kinds are untouched
@Test func appRuleMatchesTextByBundleId() {
    let rules = rules {
        $0.apps = [IgnoredApp(id: "com.google.Chrome", name: "Chrome")]
        $0.appsSync = true
        $0.appsRecord = true
    }
    let hit = rules.evaluate(
        content: .text("hello"), pasteboardTypes: [], sourceBundleId: "com.google.Chrome")
    #expect(hit == IgnoreVerdict(suppressSync: true, suppressRecord: true))

    let otherApp = rules.evaluate(
        content: .text("hello"), pasteboardTypes: [], sourceBundleId: "com.apple.Safari")
    #expect(otherApp == .none)

    let exempted = rules.evaluate(
        content: .text("hello"), pasteboardTypes: [], sourceBundleId: nil)
    #expect(exempted == .none, "nil source (a restore write) must exempt the app rule")

    let image = rules.evaluate(
        content: .image(width: 1, height: 1, rgba: [0, 0, 0, 255]),
        pasteboardTypes: [], sourceBundleId: "com.google.Chrome")
    #expect(image == .none, "The app rule acts on text only")
}

/// Type rule: any overlap with the snapshot counts, whatever the content kind
@Test func typeRuleMatchesSnapshotOverlap() {
    let rules = rules { _ in }  // defaults carry the well-known type list
    let hit = rules.evaluate(
        content: .text("secret"),
        pasteboardTypes: ["public.utf8-plain-text", "net.antelle.keeweb"],
        sourceBundleId: nil)
    #expect(hit == IgnoreVerdict(suppressSync: true, suppressRecord: false))

    let image = rules.evaluate(
        content: .image(width: 1, height: 1, rgba: [0, 0, 0, 255]),
        pasteboardTypes: ["com.typeit4me.clipping"], sourceBundleId: nil)
    #expect(image.suppressSync, "The type rule covers every content kind")

    let miss = rules.evaluate(
        content: .text("plain"), pasteboardTypes: ["public.utf8-plain-text"],
        sourceBundleId: nil)
    #expect(miss == .none)
}

/// Regex rule: patterns run exactly as written, standard search semantics —
/// a match anywhere in the text counts, user-written anchors still bind, and
/// an uncompilable pattern degrades to a literal substring check
@Test func regexRuleMatchesBySearch() {
    let rules = rules {
        $0.regexes = ["[0-9]{6}", "^secret$", "([invalid"]
        $0.regexRecord = true
    }
    #expect(
        rules.evaluate(content: .text("834721"), pasteboardTypes: [], sourceBundleId: nil)
            == IgnoreVerdict(suppressSync: true, suppressRecord: true))
    #expect(
        rules.evaluate(
            content: .text("验证码 834721"), pasteboardTypes: [], sourceBundleId: nil)
            .suppressSync,
        "Search semantics: a match anywhere in the text counts")
    #expect(
        rules.evaluate(content: .text("secret"), pasteboardTypes: [], sourceBundleId: nil)
            .suppressSync)
    #expect(
        rules.evaluate(content: .text("my secret!"), pasteboardTypes: [], sourceBundleId: nil)
            == .none,
        "User-written anchors bind as written")
    #expect(
        rules.evaluate(
            content: .text("see ([invalid here"), pasteboardTypes: [], sourceBundleId: nil)
            .suppressSync,
        "An uncompilable pattern degrades to a literal substring check")
    #expect(
        rules.evaluate(content: .text("plain"), pasteboardTypes: [], sourceBundleId: nil)
            == .none)
}

/// File rule: name globs vs full-path globs, comments and blank lines,
/// filtering semantics (hits are excluded, the rest of the batch goes
/// through), case-insensitive
@Test func fileRuleFiltersMatchedPaths() {
    let filtering = rules {
        $0.filePatterns = """
            # keys never leave this machine
            *.key

            /Users/*/secrets/*
            """
        $0.filesRecord = true
    }
    let partial = filtering.evaluate(
        content: .files(["/tmp/a.yaml", "/tmp/b.KEY"]), pasteboardTypes: [],
        sourceBundleId: nil)
    #expect(
        partial.broadcastFilesExcluded == ["/tmp/b.KEY"],
        "Only the hits are excluded (case-insensitive); the rest still syncs")
    #expect(
        partial.recordFilesExcluded == ["/tmp/b.KEY"],
        "The record toggle arms the record leg with the same hits")
    #expect(
        !partial.suppressSync && !partial.suppressRecord,
        "File rules filter; they never suppress the whole event")
    #expect(
        !filtering.evaluate(
            content: .files(["/Users/zero/secrets/token.txt"]), pasteboardTypes: [],
            sourceBundleId: nil
        ).broadcastFilesExcluded.isEmpty, "A pattern containing / matches the full path")
    #expect(
        filtering.evaluate(
            content: .files(["/tmp/notes.txt"]), pasteboardTypes: [], sourceBundleId: nil)
            == .none)
    #expect(
        filtering.evaluate(content: .text("*.key"), pasteboardTypes: [], sourceBundleId: nil)
            == .none, "The file rule leaves text alone")

    // The sync toggle off leaves the broadcast leg unarmed
    let recordOnly = rules {
        $0.filePatterns = "*.key"
        $0.filesSync = false
        $0.filesRecord = true
    }
    let verdict = recordOnly.evaluate(
        content: .files(["/tmp/b.key"]), pasteboardTypes: [], sourceBundleId: nil)
    #expect(verdict.broadcastFilesExcluded.isEmpty)
    #expect(verdict.recordFilesExcluded == ["/tmp/b.key"])
}

/// Toggle merging: hits from several kinds OR together; a kind with both
/// toggles off contributes nothing
@Test func verdictMergesAcrossRuleKinds() {
    let merged = rules {
        $0.apps = [IgnoredApp(id: "com.example.app", name: "Example")]
        $0.appsSync = false
        $0.appsRecord = true
        $0.regexes = ["^token$"]
        $0.regexSync = true
        $0.regexRecord = false
    }
    let both = merged.evaluate(
        content: .text("token"), pasteboardTypes: [], sourceBundleId: "com.example.app")
    #expect(both == IgnoreVerdict(suppressSync: true, suppressRecord: true))

    let disabled = rules {
        $0.types = ["custom.type"]
        $0.typesSync = false
        $0.typesRecord = false
    }
    #expect(
        disabled.evaluate(
            content: .text("x"), pasteboardTypes: ["custom.type"], sourceBundleId: nil)
            == .none, "Both toggles off means the rule kind is inert")
}

/// wantsSourceApp: only when the app list is non-empty and a toggle is on
@Test func wantsSourceAppReflectsConfiguration() {
    #expect(!rules { _ in }.wantsSourceApp)
    #expect(
        rules { $0.apps = [IgnoredApp(id: "a.b", name: "X")] }.wantsSourceApp)
    #expect(
        !rules {
            $0.apps = [IgnoredApp(id: "a.b", name: "X")]
            $0.appsSync = false
        }.wantsSourceApp)
}
