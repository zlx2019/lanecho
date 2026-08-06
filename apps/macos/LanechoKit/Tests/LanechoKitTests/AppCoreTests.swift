// Shell layer tests: the full two-core loopback (sync landing, both history
// entry points, incognito, restore registration, the ApplyRemote contract).
// The clipboard is injected as a fake port so the system clipboard is never
// touched (safe on CI); discovery runs on an isolated UDP port with Bonjour
// switched off.

import Foundation
import Testing

@testable import LanechoKit

/// Fake clipboard: records writes and can be primed to fail exactly once.
private final class FakeClipboard: ClipboardPort, @unchecked Sendable {
    private let lock = NSLock()
    private var texts: [String] = []
    private var failNext = false

    func writeText(_ text: String) async throws {
        let shouldFail = lock.withLock {
            let fail = failNext
            failNext = false
            return fail
        }
        if shouldFail {
            throw ClipboardError.writeFailed
        }
        lock.withLock { texts.append(text) }
    }

    func writeImage(width: Int, height: Int, rgba: [UInt8]) async throws {}

    func writeFiles(_ paths: [String]) async throws {}

    func written() -> [String] {
        lock.withLock { texts }
    }

    func failNextWrite() {
        lock.withLock { failNext = true }
    }

    /// Waits until the given text has been written.
    func waitFor(_ text: String, timeout: Duration = .seconds(10)) async throws {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if lock.withLock({ texts.contains(text) }) { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout("Clipboard write: \(text)")
    }
}

/// Collects core events.
private final class CoreEventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [CoreEvent] = []

    func append(_ event: CoreEvent) {
        lock.withLock { items.append(event) }
    }

    func count(where predicate: (CoreEvent) -> Bool) -> Int {
        lock.withLock { items.filter(predicate).count }
    }

    func waitFor(
        _ describe: String, timeout: Duration = .seconds(10),
        where predicate: (CoreEvent) -> Bool
    ) async throws {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if lock.withLock({ items.contains(where: predicate) }) { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout(describe)
    }
}

/// One test node: an assembled core, a fake clipboard and an event log.
private struct CoreNode {
    let core: AppCore
    let clipboard: FakeClipboard
    let log: CoreEventLog
    let drain: Task<Void, Never>
    let dir: URL

    static func start(discoveryPort: UInt16) async throws -> CoreNode {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("lanecho-core-\(UUID().uuidString)")
        // Random TCP port: both cores run on the same machine
        try Settings(tcpPort: 0).save(dataDir: dir)
        let clipboard = FakeClipboard()
        let (core, events) = try await AppCore.start(
            dataDir: dir, clipboard: clipboard,
            discoveryPort: discoveryPort, bonjour: false, watchClipboard: false)
        let log = CoreEventLog()
        let drain = Task { for await event in events { log.append(event) } }
        return CoreNode(core: core, clipboard: clipboard, log: log, drain: drain, dir: dir)
    }

    func stop() async {
        drain.cancel()
        await core.shutdown()
        try? FileManager.default.removeItem(at: dir)
    }
}

/// Pairs two cores: A initiates, B accepts off its event stream.
private func pairCores(_ a: CoreNode, _ b: CoreNode) async throws {
    let bFingerprint = await b.core.localInfo().fingerprint
    // Wait for mutual discovery on the shared isolated UDP port
    let deadline = ContinuousClock.now + .seconds(15)
    while ContinuousClock.now < deadline {
        if await a.core.peers().contains(where: { $0.info.fingerprint == bFingerprint }) {
            break
        }
        try await Task.sleep(for: .milliseconds(100))
    }
    // B accepts the inbound request
    let aFingerprint = await a.core.localInfo().fingerprint
    let accept = Task {
        let deadline = ContinuousClock.now + .seconds(10)
        while ContinuousClock.now < deadline {
            if b.log.count(where: {
                if case .pairRequested(let info) = $0 {
                    info.fingerprint == aFingerprint
                } else {
                    false
                }
            }) > 0 {
                await b.core.respondPair(fingerprint: aFingerprint, accept: true)
                return
            }
            try? await Task.sleep(for: .milliseconds(50))
        }
    }
    try await a.core.pair(fingerprint: bFingerprint)
    _ = await accept.value
}

/// Full path: a local copy on A lands on B's clipboard and produces a history
/// entry on both sides (local origin on A, remote origin on B).
@Test(
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(2)))
func coreLoopbackSyncAndDualHistory() async throws {
    let port = UInt16.random(in: 42600...42999)
    let a = try await CoreNode.start(discoveryPort: port)
    let b = try await CoreNode.start(discoveryPort: port)
    defer { Task { await a.stop() } }
    defer { Task { await b.stop() } }
    try await pairCores(a, b)

    // Simulate A's watcher reporting a real copy
    let text = "core-loopback-🧭 keep trailing space "
    await a.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))

    // B: the clipboard lands byte-for-byte and the history entry's origin is
    // A's device name
    try await b.clipboard.waitFor(text)
    try await b.log.waitFor("B history change") {
        if case .historyChanged = $0 { true } else { false }
    }
    let aName = await a.core.localInfo().name
    let bEntry = try #require(await b.core.historyList().first)
    #expect(bEntry.origin == aName)
    #expect(bEntry.sourceApp == nil)

    // A: the local history entry has no origin, since the copy was local
    try await a.log.waitFor("A history change") {
        if case .historyChanged = $0 { true } else { false }
    }
    let aEntry = try #require(await a.core.historyList().first)
    #expect(aEntry.origin == nil)
    // B is notified that remote content was applied
    #expect(
        b.log.count(where: {
            if case .appliedRemote(let from, _) = $0 { from == aName } else { false }
        }) == 1)
}

/// Incognito only suppresses the local history; sync keeps running.
@Test(
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(2)))
func incognitoSkipsHistoryButSyncs() async throws {
    let port = UInt16.random(in: 42600...42999)
    let a = try await CoreNode.start(discoveryPort: port)
    let b = try await CoreNode.start(discoveryPort: port)
    defer { Task { await a.stop() } }
    defer { Task { await b.stop() } }
    try await pairCores(a, b)

    await b.core.setIncognito(true)
    let text = "incognito still syncs"
    await a.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    try await b.clipboard.waitFor(text)
    // Give the history worker a beat, then confirm nothing was recorded
    try await Task.sleep(for: .milliseconds(300))
    #expect(await b.core.historyList().isEmpty, "Incognito mode must not record history")
    #expect(!(await b.clipboard.written().isEmpty), "Incognito mode must not affect sync")
}

/// Restore registration: after restoreEntry, a local copy with the same hash
/// does not re-collect the source application, so the bump keeps the original;
/// the restore itself counts as one copy and bumps the history counter.
@Test(.timeLimit(.minutes(1)))
func restorePreservesSourceApp() async throws {
    let port = UInt16.random(in: 42600...42999)
    let node = try await CoreNode.start(discoveryPort: port)
    defer { Task { await node.stop() } }

    // Inject a "local copy" straight through the engine to create the entry.
    // The pump collects the frontmost application and the test process cannot
    // control what that turns out to be, so forcing a known source application
    // is not an option; this asserts the behaviour instead: the bump after a
    // restore must not change sourceApp.
    let text = "restore preserves source"
    await node.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    try await node.log.waitFor("initial record") {
        if case .historyChanged = $0 { true } else { false }
    }
    let before = try #require(await node.core.historyList().first)

    // Restore; the fake clipboard receives it; then simulate the watcher
    // seeing that write, with the same hash
    try await node.core.restoreEntry(id: before.id)
    try await node.clipboard.waitFor(text)
    await node.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    try await node.log.waitFor("bumped record", timeout: .seconds(5)) {
        if case .historyChanged = $0 { true } else { false }
    }
    // Poll until the bump is persisted
    let deadline = ContinuousClock.now + .seconds(5)
    while ContinuousClock.now < deadline {
        if await node.core.historyList().first?.copyCount == 2 { break }
        try await Task.sleep(for: .milliseconds(50))
    }
    let after = try #require(await node.core.historyList().first)
    #expect(after.copyCount == 2, "Restore counts as one copy and must bump the entry")
    #expect(after.sourceApp == before.sourceApp, "Restore must preserve the original source app")
    #expect(after.origin == nil)
}

/// ApplyRemote contract: a failed clipboard write must cancel the echo
/// registration, so a later real local copy of the same content is not
/// swallowed by an orphan hash and still produces a local history entry.
@Test(
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(2)))
func applyRemoteFailureCancelsEcho() async throws {
    let port = UInt16.random(in: 42600...42999)
    let a = try await CoreNode.start(discoveryPort: port)
    let b = try await CoreNode.start(discoveryPort: port)
    defer { Task { await a.stop() } }
    defer { Task { await b.stop() } }
    try await pairCores(a, b)

    // B's next write fails: the engine has already registered the echo, so the
    // shell layer must cancel it
    b.clipboard.failNextWrite()
    let text = "contract-failure-path"
    await a.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    // Wait for the syncSent receipt: B's transaction has completed, the write
    // failure happens in the shell layer
    try await a.log.waitFor("delivery receipt") {
        if case .syncSent(_, let error) = $0 { error == nil } else { false }
    }
    try await Task.sleep(for: .milliseconds(300))
    #expect(await b.clipboard.written().isEmpty, "The clipboard write must have failed")
    #expect(await b.core.historyList().isEmpty, "A failed write must not enter history")

    // The user really copies the same content on B: an echo hash that was
    // never cancelled would swallow it
    await b.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    try await b.log.waitFor("B local record after echo cancellation") {
        if case .historyChanged = $0 { true } else { false }
    }
    let entry = try #require(await b.core.historyList().first)
    #expect(entry.origin == nil, "This is a real local copy, not a remote entry")
}

/// Regression: shutdown must drain the history queue, or the last batch of
/// copies is lost.
///
/// Cancelling the pump and the worker before `flush` does not work:
/// cancelling a task blocked on `for await` **does not discard elements
/// already buffered**, so the worker and flush run concurrently. Flush wins
/// the race and writes an index.json without the entry while the entry's blob
/// is already on disk, and the next startup deletes that blob as an orphan
/// during `load`. From the user's side, content they just copied vanishes.
@Test(.timeLimit(.minutes(2)))
func shutdownDrainsPendingHistoryJobs() async throws {
    let node = try await CoreNode.start(discoveryPort: UInt16.random(in: 42600...42999))
    defer { try? FileManager.default.removeItem(at: node.dir) }

    // Inject a local copy and shut down **immediately**, leaving the worker no
    // room to finish at its own pace
    let text = "last copy before shutdown"
    await node.core.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    await node.core.shutdown()
    node.drain.cancel()

    // Reload from disk: that entry must already be persisted
    let store = await HistoryStore.load(dataDir: node.dir)
    let texts = await store.list(sort: "recent").compactMap(\.text)
    #expect(texts == [text], "History jobs queued before shutdown must reach disk")
}
