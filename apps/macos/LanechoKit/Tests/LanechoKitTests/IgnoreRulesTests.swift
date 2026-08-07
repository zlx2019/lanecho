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

/// Regex rule: whole-string semantics — a partial hit is not a match, a plain
/// string is an exact comparison, and an uncompilable pattern degrades to a
/// literal instead of erroring
@Test func regexRuleMatchesWholeText() {
    let rules = rules {
        $0.regexes = ["^[a-z0-9]{8}$", "exact text", "([invalid"]
        $0.regexRecord = true
    }
    #expect(
        rules.evaluate(content: .text("abcd1234"), pasteboardTypes: [], sourceBundleId: nil)
            == IgnoreVerdict(suppressSync: true, suppressRecord: true))
    #expect(
        rules.evaluate(
            content: .text("prefix abcd1234"), pasteboardTypes: [], sourceBundleId: nil)
            == .none,
        "A partial match must not count: the whole text has to match")
    #expect(
        rules.evaluate(content: .text("exact text"), pasteboardTypes: [], sourceBundleId: nil)
            .suppressSync)
    #expect(
        rules.evaluate(content: .text("exact text!"), pasteboardTypes: [], sourceBundleId: nil)
            == .none)
    #expect(
        rules.evaluate(content: .text("([invalid"), pasteboardTypes: [], sourceBundleId: nil)
            .suppressSync,
        "An uncompilable pattern degrades to a literal full-string match")
}

/// File rule: name globs vs full-path globs, comments and blank lines, any
/// hit ignores the whole batch, case-insensitive
@Test func fileRuleParsesGitignoreSubset() {
    let rules = rules {
        $0.filePatterns = """
            # keys never leave this machine
            *.key

            /Users/*/secrets/*
            """
        $0.filesRecord = true
    }
    #expect(
        rules.evaluate(
            content: .files(["/tmp/server.KEY"]), pasteboardTypes: [], sourceBundleId: nil)
            == IgnoreVerdict(suppressSync: true, suppressRecord: true),
        "Name glob, case-insensitive")
    #expect(
        rules.evaluate(
            content: .files(["/Users/zero/secrets/token.txt"]), pasteboardTypes: [],
            sourceBundleId: nil
        ).suppressSync, "A pattern containing / matches the full path")
    #expect(
        rules.evaluate(
            content: .files(["/tmp/a.txt", "/tmp/b.key"]), pasteboardTypes: [],
            sourceBundleId: nil
        ).suppressSync, "One hit ignores the whole batch")
    #expect(
        rules.evaluate(
            content: .files(["/tmp/notes.txt"]), pasteboardTypes: [], sourceBundleId: nil)
            == .none)
    #expect(
        rules.evaluate(content: .text("*.key"), pasteboardTypes: [], sourceBundleId: nil)
            == .none, "The file rule leaves text alone")
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
