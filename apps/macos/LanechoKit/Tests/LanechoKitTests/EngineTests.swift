// Sync engine loopback tests: two real engines talking over localhost.
// Mirrors the Rust sync/tests.rs surface: pairing / sync / echo / LWW /
// sync disabled / unpaired rejection / survival across restart.

import Foundation
import Testing

@testable import LanechoKit

/// Event-stream inspector: a background task drains the stream into a
/// buffer that assertions poll.
private final class EventInspector: @unchecked Sendable {
    /// Lock-guarded buffer, shared by the drain task and the assertions.
    private final class Sink: @unchecked Sendable {
        private let lock = NSLock()
        private var events: [EngineEvent] = []
        func append(_ event: EngineEvent) {
            lock.withLock { events.append(event) }
        }
        func snapshot() -> [EngineEvent] {
            lock.withLock { events }
        }
    }

    private let sink = Sink()
    private var task: Task<Void, Never>?

    init(_ stream: AsyncStream<EngineEvent>) {
        let sink = self.sink
        task = Task {
            for await event in stream {
                sink.append(event)
            }
        }
    }

    /// Snapshot of everything seen so far.
    func all() -> [EngineEvent] {
        sink.snapshot()
    }

    /// Poll until the first event matching the predicate; 5s timeout by
    /// default.
    @discardableResult
    func waitFor(
        timeout: Duration = .seconds(5),
        _ predicate: @escaping (EngineEvent) -> Bool
    ) async throws -> EngineEvent {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if let hit = all().first(where: predicate) {
                return hit
            }
            try await Task.sleep(for: .milliseconds(20))
        }
        throw TransportError.timeout("event")
    }

    /// Wait out a quiet period, then confirm no matching event arrived;
    /// for negative assertions.
    func assertNever(
        within duration: Duration = .milliseconds(400),
        _ predicate: @escaping (EngineEvent) -> Bool
    ) async throws -> Bool {
        try await Task.sleep(for: duration)
        return !all().contains(where: predicate)
    }

    deinit { task?.cancel() }
}

/// One test engine: temporary data directory, random port.
private struct TestNode {
    let engine: SyncEngine
    let inspector: EventInspector
    let dir: URL
    let info: PeerInfo
    let port: UInt16

    static func start(
        syncMode: SyncMode = .both, syncTypes: SyncTypes = SyncTypes(files: true)
    ) async throws -> TestNode {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("lanecho-engine-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let (engine, stream) = try await SyncEngine.start(
            config: EngineConfig(dataDir: dir, tcpPort: 0, syncMode: syncMode, syncTypes: syncTypes))
        return TestNode(
            engine: engine, inspector: EventInspector(stream), dir: dir,
            info: await engine.localInfo(), port: await engine.port())
    }

    /// Inject the other node into the local peer table, standing in for
    /// the discovery layer.
    func sees(_ other: TestNode) async {
        await engine.peerUp(Peer(info: other.info, addrs: ["127.0.0.1"], port: other.port))
    }

    func cleanup() async {
        await engine.shutdown()
        try? FileManager.default.removeItem(at: dir)
    }
}

/// Complete an A→B pairing, with B accepting automatically.
private func pairNodes(_ a: TestNode, _ b: TestNode) async throws {
    await a.sees(b)
    await b.sees(a)
    let pairTask = Task { try await a.engine.pair(fingerprint: b.info.fingerprint) }
    try await b.inspector.waitFor { event in
        if case .pairRequested(let peer) = event { return peer.fingerprint == a.info.fingerprint }
        return false
    }
    await b.engine.respondPair(fingerprint: a.info.fingerprint, accept: true)
    try await pairTask.value
}

/// Pairing (accepted) → both sides enter the paired list → sync delivers
/// byte-for-byte, plus a delivery receipt.
@Test(.timeLimit(.minutes(1)))
func pairThenSyncEndToEnd() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }

    try await pairNodes(a, b)
    #expect(await a.engine.pairedList().map(\.fingerprint) == [b.info.fingerprint])
    #expect(await b.engine.pairedList().map(\.fingerprint) == [a.info.fingerprint])

    let text = "  cross-device text\n\t🛰 \u{7f} preserve exactly  "
    await a.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    let applied = try await b.inspector.waitFor { event in
        if case .applyRemote = event { return true }
        return false
    }
    guard case .applyRemote(let received, let from, _, _) = applied else { return }
    #expect(received == .text(text))
    #expect(from.fingerprint == a.info.fingerprint)
    try await a.inspector.waitFor { event in
        if case .syncSent(_, let error) = event { return error == nil }
        return false
    }
}

/// Peer rejects the pairing: the initiator throws pairRejected and neither
/// side ends up in the paired list.
@Test(.timeLimit(.minutes(1)))
func pairRejectionLeavesBothUnpaired() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }

    await a.sees(b)
    let pairTask = Task { try await a.engine.pair(fingerprint: b.info.fingerprint) }
    try await b.inspector.waitFor { event in
        if case .pairRequested = event { return true }
        return false
    }
    await b.engine.respondPair(fingerprint: a.info.fingerprint, accept: false)
    await #expect(throws: TransportError.self) { try await pairTask.value }
    #expect(await a.engine.pairedList().isEmpty)
    #expect(await b.engine.pairedList().isEmpty)
}

/// Echo suppression: when remote-written content loops back through the
/// local watcher, nothing is broadcast and no localCopied is emitted.
@Test(.timeLimit(.minutes(1)))
func echoIsSuppressed() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    let text = "echo-me"
    await a.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hashText(text), timestampMs: nowMs()))
    let applied = try await b.inspector.waitFor { event in
        if case .applyRemote = event { return true }
        return false
    }
    guard case .applyRemote(_, _, _, let hash) = applied else { return }
    // The watcher loops back what the shell layer wrote: the engine must
    // swallow it whole
    await b.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hash, timestampMs: nowMs()))
    #expect(
        try await b.inspector.assertNever { event in
            if case .localCopied = event { return true }
            return false
        })
    // And it must not bounce back to A
    #expect(
        try await a.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })
}

/// LWW: a newer local copy makes the engine ignore a stale remote sync; it
/// still returns Ack, so the sender counts the send as a success.
@Test(.timeLimit(.minutes(1)))
func lwwIgnoresStaleRemote() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    let now = nowMs()
    // B copies first (newer) with broadcasting off: only the LWW baseline
    // moves
    await b.engine.setSyncMode(.off)
    await b.engine.clipboardChanged(
        ClipboardEvent(content: .text("newer"), hash: hashText("newer"), timestampMs: now))
    await b.engine.setSyncMode(.both)
    // A syncs over with an older timestamp
    await a.engine.clipboardChanged(
        ClipboardEvent(content: .text("older"), hash: hashText("older"), timestampMs: now - 1000))
    // The sender gets a success receipt; the peer need not distinguish
    // "applied" from "ignored"
    try await a.inspector.waitFor { event in
        if case .syncSent(_, let error) = event { return error == nil }
        return false
    }
    // The receiver does not apply it
    #expect(
        try await b.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })
}

/// Receiver has sync disabled: the disabled reason code travels back to the
/// sender unchanged.
@Test(.timeLimit(.minutes(1)))
func disabledReceiverRejectsWithReason() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    await b.engine.setSyncMode(.off)
    await a.engine.clipboardChanged(
        ClipboardEvent(content: .text("x"), hash: hashText("x"), timestampMs: nowMs()))
    let sent = try await a.inspector.waitFor { event in
        if case .syncSent = event { return true }
        return false
    }
    guard case .syncSent(_, let error) = sent else { return }
    #expect(error == ReasonCode.disabled)
}

/// An unpaired source is rejected by the engine's accept path: this dials
/// the real listener directly, bypassing the engine's peer table.
@Test(.timeLimit(.minutes(1)))
func unpairedSourceIsRejected() async throws {
    let b = try await TestNode.start()
    defer { Task { await b.cleanup() } }

    // A stranger identity that was never paired dials in directly
    let strangerDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-stranger-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: strangerDir) }
    let stranger = try DeviceIdentity.loadOrCreate(dir: strangerDir)
    let strangerTLS = try TLSContexts(material: stranger.material)

    let channel = try await dialPeer(
        addrs: ["127.0.0.1"], port: b.port,
        clientContext: strangerTLS.client, expectedFingerprint: b.info.fingerprint)
    do {
        try await syncTransaction(
            channel: channel, localInfo: stranger.peerInfo(),
            expectedFingerprint: b.info.fingerprint,
            sync: .clipboardSync(
                seq: 0, timestampMs: nowMs(), contentType: ContentType.text, data: "x"))
        Issue.record("An unpaired source must be rejected")
    } catch let TransportError.rejected(code) {
        #expect(code == ReasonCode.notPaired)
    }
}

/// Sender has sync disabled: localCopied still fires, so the history
/// pipeline is unaffected, but nothing is broadcast.
@Test(.timeLimit(.minutes(1)))
func disabledSenderDoesNotBroadcast() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    await a.engine.setSyncMode(.off)
    await a.engine.clipboardChanged(
        ClipboardEvent(content: .text("quiet"), hash: hashText("quiet"), timestampMs: nowMs()))
    try await a.inspector.waitFor { event in
        if case .localCopied = event { return true }
        return false
    }
    #expect(
        try await b.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })
}

/// Pairings persist to paired.json and survive an engine restart.
@Test(.timeLimit(.minutes(1)))
func pairedSurvivesRestart() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await b.cleanup() } }
    try await pairNodes(a, b)
    await a.engine.shutdown()

    let (reborn, _) = try await SyncEngine.start(
        config: EngineConfig(dataDir: a.dir, tcpPort: 0))
    let list = await reborn.pairedList()
    #expect(list.map(\.fingerprint) == [b.info.fingerprint])
    await reborn.shutdown()
    try? FileManager.default.removeItem(at: a.dir)
}

/// Regression: an echo registration nobody consumes has to expire, or it
/// swallows a genuine local copy.
///
/// The only consumer of an echo registration is an event from the watcher.
/// But the watcher dedups on "the stamp moved, the content did not" — when
/// what the shell layer writes is byte-for-byte identical to what is
/// already on the clipboard, **no event is produced at all**, so the
/// registration is never consumed and there is no failure branch to undo
/// it. It becomes an orphan. An orphan that never expires stays stuck and
/// swallows the user's next real copy of that same text whole: no
/// broadcast, no history entry, and the LWW baseline does not advance, so
/// the peer's older content can still overwrite it.
///
/// The orphan is created through the real accept path `acceptSync` — the
/// only production entry point for pushEcho — and the matching clipboard
/// event is then deliberately **not** delivered, reproducing exactly the
/// "written content equals existing content" case.
@Test(.timeLimit(.minutes(2)))
func orphanEchoExpiresInsteadOfSwallowingRealCopy() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    let text = "Remote text exactly matches local text"
    let hash = hashText(text)

    // Accept one remote sync: registers the echo and asks the shell layer
    // to apply it
    let rejection = await b.engine.acceptSync(
        remote: a.info, timestampMs: nowMs(), contentKind: ContentType.text, data: text)
    #expect(rejection == nil, "Text sync from a paired peer must be accepted")
    try await b.inspector.waitFor { event in
        if case .applyRemote(_, _, _, let h) = event { return h == hash }
        return false
    }
    // Deliberately no clipboardChanged: simulates "the written content
    // equals what is already on the clipboard, so the watcher dedups and
    // emits nothing" — the registration is now an orphan

    // Wait for the orphan to expire
    try await Task.sleep(for: Config.echoTTL + .milliseconds(500))

    // The user really copies that same text: it must still be handled as a
    // local copy, not swallowed
    await b.engine.clipboardChanged(
        ClipboardEvent(content: .text(text), hash: hash, timestampMs: nowMs()))
    try await b.inspector.waitFor { event in
        if case .localCopied(_, let h, _, _) = event { return h == hash }
        return false
    }
}

/// Regression: repeated pairing requests from the same peer prompt only
/// once.
///
/// If every `decidePair` unconditionally yields `.pairRequested`, the shell
/// layer puts up a **modal** NSAlert each time, so a peer that keeps
/// reconnecting and re-sending pairRequest — buggy retry or malicious — can
/// trap the user behind an endless stack of dialogs. It also spawns one
/// uncancellable 300s timeout task per request, each strongly retaining the
/// whole engine.
@Test(.timeLimit(.minutes(1)))
func repeatedPairRequestsPromptOnlyOnce() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }

    // First request: suspends waiting for the user's decision, and prompts
    let first = Task { await b.engine.decidePair(remote: a.info) }
    try await b.inspector.waitFor { event in
        if case .pairRequested(let peer) = event { return peer.fingerprint == a.info.fingerprint }
        return false
    }

    // Two more from the same peer: each supersedes the previous one, and
    // must not prompt again
    let second = Task { await b.engine.decidePair(remote: a.info) }
    _ = await first.value  // the superseded one resumes as a rejection
    let third = Task { await b.engine.decidePair(remote: a.info) }
    _ = await second.value
    try await Task.sleep(for: .milliseconds(200))

    let prompts = b.inspector.all().filter {
        if case .pairRequested = $0 { return true }
        return false
    }
    #expect(prompts.count == 1, "Repeated requests from one peer must prompt once; got \(prompts.count)")

    // Cleanup: answer the last request so no continuation is left dangling
    await b.engine.respondPair(fingerprint: a.info.fingerprint, accept: false)
    _ = await third.value
}

// MARK: - Blob sync loopback (images/files; mirrors Rust sync/tests.rs)

/// Build a deterministic, fully opaque test image: ImageIO rewrites
/// semi-transparent pixels through premultiplied alpha (see BlobTests).
private func sampleImage(width: Int = 24, height: Int = 16) -> ClipboardContent {
    var rgba = [UInt8]()
    rgba.reserveCapacity(width * height * 4)
    for pixel in 0..<width * height {
        rgba.append(contentsOf: [
            UInt8(pixel * 7 % 251), UInt8(pixel * 11 % 241), UInt8(pixel * 13 % 239), 0xFF,
        ])
    }
    return .image(width: width, height: height, rgba: rgba)
}

/// Inject one "local copy": content plus a freshly computed hash.
private func inject(_ node: TestNode, _ content: ClipboardContent, at: UInt64 = nowMs()) async {
    await node.engine.clipboardChanged(
        ClipboardEvent(content: content, hash: content.hash(), timestampMs: at))
}

/// Image end to end: A copies → B lands byte-for-byte identical RGBA, and
/// the echo does not bounce back.
@Test(.timeLimit(.minutes(1)))
func imageSyncEndToEnd() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    let image = sampleImage()
    await inject(a, image)
    let applied = try await b.inspector.waitFor(timeout: .seconds(15)) { event in
        if case .applyRemote(.image(_, _, _), _, _, _) = event { return true }
        return false
    }
    guard case .applyRemote(let content, let from, _, let hash) = applied else { return }
    // Echo: the watcher loops back what the shell layer wrote to the
    // clipboard; B must swallow it whole and not bounce it back to A.
    // **Must follow applyRemote immediately**: the echo registration TTL is
    // only 2s (in production the watcher loops back within one poll
    // interval), and under parallel test load a few extra steps let the
    // registration expire first — the test would then cover orphan
    // reclamation instead of echo suppression
    await b.engine.clipboardChanged(
        ClipboardEvent(content: content, hash: hash, timestampMs: nowMs()))
    #expect(content == image, "Cross-device PNG coding must restore byte-identical RGBA")
    #expect(from.fingerprint == a.info.fingerprint)
    #expect(hash == image.hash(), "The echo hash must use RGBA, matching history deduplication")
    try await a.inspector.waitFor { event in
        if case .syncSent(_, let error) = event { return error == nil }
        return false
    }
    #expect(
        try await b.inspector.assertNever { event in
            if case .localCopied(.image(_, _, _), _, _, _) = event { return true }
            return false
        })
    #expect(
        try await a.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })
}

/// Files end to end: they land under B's sync-files byte-for-byte
/// identical, and same-name collisions within a batch get a counter suffix.
@Test(.timeLimit(.minutes(1)))
func filesSyncEndToEnd() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    // Same file name in two different directories: landing resolves the
    // collision with a " (1)" suffix
    let srcA = a.dir.appendingPathComponent("x")
    let srcB = a.dir.appendingPathComponent("y")
    try FileManager.default.createDirectory(at: srcA, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: srcB, withIntermediateDirectories: true)
    let one = srcA.appendingPathComponent("same.txt")
    let two = srcB.appendingPathComponent("same.txt")
    try Data("first payload-🚀".utf8).write(to: one)
    try Data("second payload".utf8).write(to: two)

    await inject(a, .files([one.path, two.path]))
    let applied = try await b.inspector.waitFor(timeout: .seconds(15)) { event in
        if case .applyRemote(.files(_), _, _, _) = event { return true }
        return false
    }
    guard case .applyRemote(.files(let landed), _, _, let hash) = applied else { return }
    #expect(landed.count == 2)
    let syncRoot = b.dir.appendingPathComponent(syncFilesDirName).path + "/"
    #expect(landed.allSatisfy { $0.hasPrefix(syncRoot) }, "Files must land under the receiver's sync-files root")
    #expect(
        landed.map { URL(fileURLWithPath: $0).lastPathComponent } == [
            "same.txt", "same (1).txt",
        ])
    #expect(try Data(contentsOf: URL(fileURLWithPath: landed[0])) == Data("first payload-🚀".utf8))
    #expect(try Data(contentsOf: URL(fileURLWithPath: landed[1])) == Data("second payload".utf8))
    #expect(hash == ClipboardContent.files(landed).hash(), "The echo hash must use local landed paths")
}

/// Type toggles: with images off on the receiver, the offer is rejected
/// with unsupported_type before any stream is read.
@Test(.timeLimit(.minutes(1)))
func imageOfferRejectedWhenTypeDisabled() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    await b.engine.setSyncTypes(SyncTypes(text: true, images: false, files: true))
    await inject(a, sampleImage())
    let sent = try await a.inspector.waitFor { event in
        if case .syncSent = event { return true }
        return false
    }
    guard case .syncSent(_, let error) = sent else { return }
    #expect(error == ReasonCode.unsupportedType)
    #expect(
        try await b.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })
}

/// Sync direction policy: a send-only peer rejects inbound sync with
/// disabled, but still broadcasts its own copies.
@Test(.timeLimit(.minutes(1)))
func sendOnlyPeerRejectsInboundButStillBroadcasts() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start(syncMode: .send)
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    // A → B: rejected by B's direction gate
    await inject(a, .text("cannot arrive"))
    let sent = try await a.inspector.waitFor { event in
        if case .syncSent = event { return true }
        return false
    }
    guard case .syncSent(_, let error) = sent else { return }
    #expect(error == ReasonCode.disabled)

    // B → A: delivered as usual
    await inject(b, .text("can be sent"))
    try await a.inspector.waitFor { event in
        if case .applyRemote(.text("can be sent"), _, _, _) = event { return true }
        return false
    }
}

/// Sync direction policy: a receive-only peer never broadcasts its own
/// copies, but inbound sync still lands.
@Test(.timeLimit(.minutes(1)))
func receiveOnlyPeerDoesNotBroadcast() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start(syncMode: .receive)
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    await inject(b, .text("silent local copy"))
    #expect(
        try await a.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })

    await inject(a, .text("one-way reachable"))
    try await b.inspector.waitFor { event in
        if case .applyRemote(.text("one-way reachable"), _, _, _) = event { return true }
        return false
    }
}

/// Size cap: an oversize file offer is rejected before the stream is read,
/// leaving nothing behind on the receiver.
@Test(.timeLimit(.minutes(1)))
func oversizeFilesRejectedBeforeStream() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start()
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    await b.engine.setMaxSyncFileBytes(4)
    let big = a.dir.appendingPathComponent("big.bin")
    try Data(repeating: 0xAB, count: 64).write(to: big)
    await inject(a, .files([big.path]))
    let sent = try await a.inspector.waitFor { event in
        if case .syncSent = event { return true }
        return false
    }
    guard case .syncSent(_, let error) = sent else { return }
    #expect(error == ReasonCode.tooLarge)
    // Rejected at the offer stage: no batch directory may be left on the
    // receiver's disk
    let residue =
        (try? FileManager.default.contentsOfDirectory(
            atPath: b.dir.appendingPathComponent(syncFilesDirName).path)) ?? []
    #expect(residue.isEmpty, "Rejection before streaming must leave no batch directory")
}

/// LWW runs first: when the receiver's clipboard is newer, a blob offer is
/// Acked outright and no stream is read.
@Test(.timeLimit(.minutes(1)))
func staleBlobOfferAckedWithoutTransfer() async throws {
    let a = try await TestNode.start()
    let b = try await TestNode.start(syncMode: .receive)
    defer { Task { await a.cleanup(); await b.cleanup() } }
    try await pairNodes(a, b)

    let now = nowMs()
    // B copies first (newer; receive mode does not broadcast, it only moves
    // the LWW baseline)
    await inject(b, .text("newer local content"), at: now)
    // A sends an image with an older timestamp
    await inject(a, sampleImage(), at: now - 1_000)
    // The sender gets a success receipt: Ack means success, no need to
    // distinguish "applied" from "ignored"
    try await a.inspector.waitFor { event in
        if case .syncSent(_, let error) = event { return error == nil }
        return false
    }
    #expect(
        try await b.inspector.assertNever { event in
            if case .applyRemote = event { return true }
            return false
        })
}
