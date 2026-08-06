// Session transaction loopback tests: a real dial against a real listener,
// with the engine's decisions injected as a stub. Mirrors the Rust sync
// loopback test surface: pair accept/reject, sync delivery and rejection
// codes, unpair notification, byte-for-byte text.

import Foundation
import Testing

@testable import LanechoKit

/// Listener-side decision stub with configurable answers; records everything
/// it receives.
private final class StubDelegate: ServerSessionDelegate, @unchecked Sendable {
    private let lock = NSLock()
    private var pairAnswer: Bool
    private var syncRejection: String?
    private var receivedTexts: [String] = []
    private var pairRequests: [String] = []
    private var unpairedFingerprints: [String] = []

    init(pairAnswer: Bool = true, syncRejection: String? = nil) {
        self.pairAnswer = pairAnswer
        self.syncRejection = syncRejection
    }

    func decidePair(remote: PeerInfo) async -> Bool {
        lock.withLock {
            pairRequests.append(remote.fingerprint)
            return pairAnswer
        }
    }

    func acceptSync(
        remote: PeerInfo, timestampMs: UInt64, contentKind: String, data: String
    ) async -> String? {
        lock.withLock {
            if let rejection = syncRejection { return rejection }
            receivedTexts.append(data)
            return nil
        }
    }

    // Blob acceptance is out of scope here (the engine loopback tests drive the
    // real implementation): always reject
    func decideBlobOffer(remote: PeerInfo, timestampMs: UInt64, offer: BlobOffer) async
        -> OfferDecision
    { .reject(ReasonCode.unsupportedType) }

    func finishBlobImage(remote: PeerInfo, timestampMs: UInt64, png: Data) async -> String? {
        ReasonCode.unsupportedType
    }

    func finishBlobFiles(
        remote: PeerInfo, timestampMs: UInt64, parts: [URL], metas: [FileMeta], batchDir: URL
    ) async -> String? {
        ReasonCode.unsupportedType
    }

    func unpaired(remote: PeerInfo) async {
        lock.withLock {
            unpairedFingerprints.append(remote.fingerprint)
        }
    }

    var texts: [String] {
        lock.lock()
        defer { lock.unlock() }
        return receivedTexts
    }
    var unpairs: [String] {
        lock.lock()
        defer { lock.unlock() }
        return unpairedFingerprints
    }
}

/// One endpoint: identity material, TLS contexts and PeerInfo.
private struct Endpoint {
    let material: IdentityMaterial
    let tls: TLSContexts
    let info: PeerInfo

    init(cert: String, key: String, name: String) throws {
        let certURL = try #require(
            Bundle.module.url(forResource: "Fixtures/\(cert)", withExtension: nil))
        let keyURL = try #require(
            Bundle.module.url(forResource: "Fixtures/\(key)", withExtension: nil))
        material = try IdentityMaterial(
            certDER: [UInt8](Data(contentsOf: certURL)),
            keyDER: [UInt8](Data(contentsOf: keyURL)))
        tls = try TLSContexts(material: material)
        info = PeerInfo(
            deviceId: name, name: name, fingerprint: material.fingerprint, platform: "macos")
    }
}

/// Shared scaffolding: start a listener, then run one dial transaction
/// against it.
private func withListener<T>(
    delegate: StubDelegate,
    _ body: (_ server: Endpoint, _ client: Endpoint, _ port: UInt16) async throws -> T
) async throws -> T {
    let server = try Endpoint(cert: "cert.der", key: "key.der", name: "server")
    let client = try Endpoint(cert: "cert2.der", key: "key2.der", name: "client")
    let (port, task) = try await startSyncListener(
        localInfo: server.info, serverContext: server.tls.server, port: 0,
        syncFilesRoot: FileManager.default.temporaryDirectory
            .appendingPathComponent("lanecho-session-sync-\(UUID().uuidString)"),
        delegate: delegate)
    defer { task.cancel() }
    return try await body(server, client, port)
}

/// Full pair-then-sync path: both transactions succeed and the text arrives
/// byte-for-byte.
@Test(.timeLimit(.minutes(1)))
func pairThenSyncDeliversByteExactText() async throws {
    let delegate = StubDelegate()
    try await withListener(delegate: delegate) { server, client, port in
        // Pair: the returned peer info is the server's identity
        let pairChannel = try await dialPeer(
            addrs: ["127.0.0.1"], port: port,
            clientContext: client.tls.client,
            expectedFingerprint: server.material.fingerprint)
        let remote = try await pairTransaction(
            channel: pairChannel, localInfo: client.info,
            expectedFingerprint: server.material.fingerprint)
        #expect(remote.fingerprint == server.material.fingerprint)

        // Sync: text deliberately carrying leading and trailing whitespace, a
        // control character and an emoji
        let text = "  sync content\n\t🚀 \u{7f} tail  "
        let syncChannel = try await dialPeer(
            addrs: ["127.0.0.1"], port: port,
            clientContext: client.tls.client,
            expectedFingerprint: server.material.fingerprint)
        try await syncTransaction(
            channel: syncChannel, localInfo: client.info,
            expectedFingerprint: server.material.fingerprint,
            sync: .clipboardSync(
                seq: 1, timestampMs: 1_754_000_000_000,
                contentType: ContentType.text, data: text))
        #expect(delegate.texts == [text])
    }
}

/// The remote user declines the pairing: pairTransaction must throw
/// pairRejected.
@Test(.timeLimit(.minutes(1)))
func pairRejectionSurfacesToDialer() async throws {
    let delegate = StubDelegate(pairAnswer: false)
    try await withListener(delegate: delegate) { server, client, port in
        let channel = try await dialPeer(
            addrs: ["127.0.0.1"], port: port,
            clientContext: client.tls.client,
            expectedFingerprint: server.material.fingerprint)
        await #expect(throws: TransportError.self) {
            _ = try await pairTransaction(
                channel: channel, localInfo: client.info,
                expectedFingerprint: server.material.fingerprint)
        }
    }
}

/// Sync rejected: the structured reason code reaches the dialer unchanged.
@Test(.timeLimit(.minutes(1)))
func syncRejectionCarriesReasonCode() async throws {
    let delegate = StubDelegate(syncRejection: ReasonCode.notPaired)
    try await withListener(delegate: delegate) { server, client, port in
        let channel = try await dialPeer(
            addrs: ["127.0.0.1"], port: port,
            clientContext: client.tls.client,
            expectedFingerprint: server.material.fingerprint)
        do {
            try await syncTransaction(
                channel: channel, localInfo: client.info,
                expectedFingerprint: server.material.fingerprint,
                sync: .clipboardSync(
                    seq: 0, timestampMs: 0, contentType: ContentType.text, data: "x"))
            Issue.record("Sync should be rejected")
        } catch let TransportError.rejected(code) {
            #expect(code == ReasonCode.notPaired)
        }
    }
}

/// The unpair notification reaches the listener-side delegate.
@Test(.timeLimit(.minutes(1)))
func unpairNotifiesDelegate() async throws {
    let delegate = StubDelegate()
    try await withListener(delegate: delegate) { server, client, port in
        let channel = try await dialPeer(
            addrs: ["127.0.0.1"], port: port,
            clientContext: client.tls.client,
            expectedFingerprint: server.material.fingerprint)
        try await unpairTransaction(
            channel: channel, localInfo: client.info,
            expectedFingerprint: server.material.fingerprint)
        // unpair has no reply, so give the listener a beat to process it
        try await Task.sleep(for: .milliseconds(200))
        #expect(delegate.unpairs == [client.material.fingerprint])
    }
}

/// A dead address at the head of the candidate list is skipped and the dial
/// succeeds on a later one.
@Test(.timeLimit(.minutes(1)))
func dialFallsThroughDeadAddresses() async throws {
    let delegate = StubDelegate()
    try await withListener(delegate: delegate) { server, client, port in
        let channel = try await dialPeer(
            addrs: ["203.0.113.1", "127.0.0.1"], port: port,
            clientContext: client.tls.client,
            expectedFingerprint: server.material.fingerprint)
        _ = try await pairTransaction(
            channel: channel, localInfo: client.info,
            expectedFingerprint: server.material.fingerprint)
    }
}

/// Regression: inbound connections are capped, and connections past the cap
/// are closed immediately.
///
/// The listener accepts any client certificate (the pairing decision lives in
/// the application layer), so a TLS handshake completes **without pairing**.
/// Without `Config.maxConcurrentConnections` actually being enforced, the task
/// group adds every inbound connection unconditionally: any host on the LAN
/// can connect and never send Hello, each one holding a slot for the full 30s
/// handshake timeout, so a modest connection rate exhausts the fd budget. The
/// Rust side guards the same way with
/// `Semaphore::new(MAX_CONCURRENT_CONNECTIONS)`.
@Test(.timeLimit(.minutes(1)))
func listenerRejectsConnectionsBeyondQuota() async throws {
    let server = try Endpoint(cert: "cert.der", key: "key.der", name: "server")
    let client = try Endpoint(cert: "cert2.der", key: "key2.der", name: "client")
    let delegate = StubDelegate()
    let (port, task) = try await startSyncListener(
        localInfo: server.info, serverContext: server.tls.server, port: 0,
        syncFilesRoot: FileManager.default.temporaryDirectory
            .appendingPathComponent("lanecho-session-sync-\(UUID().uuidString)"),
        delegate: delegate, maxConnections: 2)
    defer { task.cancel() }

    // Fill the quota with two connections that stay open and never send Hello
    // (slow-loris)
    var held: [FrameChannel] = []
    for _ in 0..<2 {
        held.append(
            try await dialPeer(
                addrs: ["127.0.0.1"], port: port, clientContext: client.tls.client,
                expectedFingerprint: server.material.fingerprint))
    }
    // Let the listener register both against the quota
    try await Task.sleep(for: .milliseconds(300))

    // Third one: TCP/TLS still connects (the cap is enforced on the listener
    // side) but is closed immediately, so the handshake never reads HelloAck
    // and surfaces as the peer hanging up
    let third = try await dialPeer(
        addrs: ["127.0.0.1"], port: port, clientContext: client.tls.client,
        expectedFingerprint: server.material.fingerprint)
    await #expect(throws: (any Error).self, "A connection at capacity must not enter a transaction") {
        try await syncTransaction(
            channel: third, localInfo: client.info,
            expectedFingerprint: server.material.fingerprint,
            sync: .clipboardSync(
                seq: 1, timestampMs: 1_754_000_000_000,
                contentType: ContentType.text, data: "over quota"))
    }
    #expect(delegate.texts.isEmpty, "Content from an over-quota connection must not be accepted")

    // Once a slot is released, new connections are accepted again
    try await held.removeLast().executeThenClose { _, outbound in outbound.finish() }
    try await Task.sleep(for: .milliseconds(300))
    let revived = try await dialPeer(
        addrs: ["127.0.0.1"], port: port, clientContext: client.tls.client,
        expectedFingerprint: server.material.fingerprint)
    try await syncTransaction(
        channel: revived, localInfo: client.info,
        expectedFingerprint: server.material.fingerprint,
        sync: .clipboardSync(
            seq: 2, timestampMs: 1_754_000_000_001,
                contentType: ContentType.text, data: "capacity released"))
    #expect(delegate.texts == ["capacity released"], "Accepting must resume after capacity is released")

    for channel in held {
        try? await channel.executeThenClose { _, outbound in outbound.finish() }
    }
}
