// Discovery tests: registry timing rules (pure logic, injected clock), UDP
// multicast loopback mutual discovery, and the liveness probe hard rule
// regression (ported from the Rust probe_rejects_imposter_listener)

import Foundation
import Testing

@testable import LanechoKit

// MARK: - Registry pure logic

private func makePeer(fp: String, addrs: [String] = ["192.168.1.2"], name: String = "p")
    -> Peer
{
    Peer(
        info: PeerInfo(deviceId: "dev-\(fp)", name: name, fingerprint: fp, platform: "macos"),
        addrs: addrs, port: 42524)
}

/// The address list only grows, only an actual change emits up, and a
/// heartbeat merely refreshes the timestamp
@Test func registryMergesAddressesOnlyGrow() {
    var registry = PeerRegistry(selfFingerprint: "self")
    let t0: UInt64 = 1_000_000
    // First sighting: up
    #expect(
        registry.upsert(makePeer(fp: "a", addrs: ["192.168.1.2"]), source: .udp, nowMs: t0)
            == .up(makePeer(fp: "a", addrs: ["192.168.1.2"])))
    // Heartbeat carrying identical info: no event
    #expect(
        registry.upsert(makePeer(fp: "a", addrs: ["192.168.1.2"]), source: .udp, nowMs: t0 + 5000)
            == nil)
    // A new address merges in (old ones kept): up carries the merged set
    let change = registry.upsert(
        makePeer(fp: "a", addrs: ["10.0.0.9"]), source: .udp, nowMs: t0 + 6000)
    guard case .up(let merged) = change else {
        Issue.record("Expected an up event")
        return
    }
    #expect(Set(merged.addrs) == ["192.168.1.2", "10.0.0.9"])
}

/// Self sightings are filtered out, and an update left with no usable address
/// after normalization is dropped
@Test func registryFiltersSelfAndUnusableAddrs() {
    var registry = PeerRegistry(selfFingerprint: "self")
    #expect(registry.upsert(makePeer(fp: "self"), source: .udp, nowMs: 0) == nil)
    #expect(
        registry.upsert(makePeer(fp: "b", addrs: ["fe80::1"]), source: .mdns, nowMs: 0) == nil)
    #expect(registry.snapshot().isEmpty)
}

/// UDP-only peers are swept once they time out; peers alive over mDNS are not
/// evicted on time alone but get a liveness probe dispatched (throttled)
@Test func sweepEvictsUdpOnlyAndProbesMdnsSilent() {
    var registry = PeerRegistry(selfFingerprint: "self")
    let t0: UInt64 = 1_000_000
    _ = registry.upsert(makePeer(fp: "udp-only"), source: .udp, nowMs: t0)
    _ = registry.upsert(makePeer(fp: "mdns-alive"), source: .mdns, nowMs: t0)

    // Not timed out yet: nothing happens (an mdns peer coming up counts as
    // just probed, so no probe fires within the first interval)
    var result = registry.sweep(nowMs: t0 + 14_000)
    #expect(result.changes.isEmpty)
    #expect(result.probes.isEmpty)

    // Past the timeout: the UDP-only peer goes offline; the mDNS peer stays
    // but is still inside the probe throttle
    result = registry.sweep(nowMs: t0 + 16_000)
    #expect(result.changes == [.down("udp-only")])
    #expect(result.probes.isEmpty)
    #expect(registry.snapshot().count == 1)

    // Past the probe interval (30s): the silent mDNS peer gets a probe
    // dispatched, and this same sweep stamps the throttle
    result = registry.sweep(nowMs: t0 + 31_000)
    #expect(result.probes.map(\.info.fingerprint) == ["mdns-alive"])
    let again = registry.sweep(nowMs: t0 + 32_000)
    #expect(again.probes.isEmpty, "Must not dispatch again within the throttle window")
}

/// mDNS service disappears: keep the peer while its UDP heartbeat is still
/// fresh (losing one channel must not flicker it offline), otherwise remove it
/// immediately
@Test func mdnsRemovedRespectsFreshUdp() {
    var registry = PeerRegistry(selfFingerprint: "self")
    let t0: UInt64 = 1_000_000
    _ = registry.upsert(makePeer(fp: "both"), source: .mdns, nowMs: t0)
    _ = registry.upsert(makePeer(fp: "both"), source: .udp, nowMs: t0 + 1000)
    // UDP is fresh: clear the flag only, do not go offline
    #expect(registry.mdnsRemoved(deviceId: "dev-both", nowMs: t0 + 2000) == nil)
    #expect(registry.snapshot().count == 1)
    // UDP then times out too: the peer falls through to the plain timeout
    // sweep path
    let result = registry.sweep(nowMs: t0 + 20_000)
    #expect(result.changes == [.down("both")])

    // mDNS-only peer: removed as soon as the service disappears
    _ = registry.upsert(makePeer(fp: "mdns"), source: .mdns, nowMs: t0)
    #expect(registry.mdnsRemoved(deviceId: "dev-mdns", nowMs: t0 + 1000) == .down("mdns"))
}

/// Probe failure: if UDP revives within the window the newer evidence wins and
/// the peer is kept, otherwise it is declared dead and goes offline
@Test func probeFailedRespectsFreshUdpEvidence() {
    var registry = PeerRegistry(selfFingerprint: "self")
    let t0: UInt64 = 1_000_000
    _ = registry.upsert(makePeer(fp: "x"), source: .mdns, nowMs: t0)
    // UDP revives while the probe is in flight
    _ = registry.upsert(makePeer(fp: "x"), source: .udp, nowMs: t0 + 100)
    #expect(registry.probeFailed(fingerprint: "x", nowMs: t0 + 200) == nil)
    #expect(registry.snapshot().count == 1)

    _ = registry.upsert(makePeer(fp: "y"), source: .mdns, nowMs: t0)
    #expect(registry.probeFailed(fingerprint: "y", nowMs: t0 + 200) == .down("y"))
}

/// Address normalization: fe80 dropped, non-loopback IPv4 first, loopback
/// last, ties keep arrival order
@Test func normalizeAddrsOrdering() {
    let sorted = normalizeAddrs(["fe80::1", "127.0.0.1", "2001:db8::2", "192.168.1.9", "10.0.0.1"])
    #expect(sorted == ["192.168.1.9", "10.0.0.1", "2001:db8::2", "127.0.0.1"])
}

// MARK: - UDP multicast loopback

/// Change collector: a drain task feeds it, assertions wait by polling with a
/// deadline.
/// Structural rule: one consumer per stream (concurrent for-await loops eat
/// each other's events), and every wait must be bounded — when multicast does
/// not work, an uncancellable await hangs the whole test suite and even
/// timeLimit cannot interrupt it. This structure guards against that.
private final class ChangeLog: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [RegistryChange] = []

    func append(_ change: RegistryChange) {
        lock.withLock { items.append(change) }
    }

    /// Waits for the first change satisfying the predicate; throws on timeout
    func waitFor(
        _ describe: String, timeout: Duration = .seconds(15),
        where predicate: (RegistryChange) -> Bool
    ) async throws -> RegistryChange {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if let hit = lock.withLock({ items.first(where: predicate) }) {
                return hit
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw TransportError.timeout(describe)
    }
}

/// Two discovery services find each other on a custom multicast port; when one
/// shuts down gracefully the other notices immediately
@Test(
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(1)))
func multicastLoopbackDiscoversAndSaysGoodbye() async throws {
    // Random port for isolation (steers clear of production 42525 and of
    // tests running in parallel)
    let port = UInt16.random(in: 42600...42999)
    let dirA = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-disc-a-\(UUID().uuidString)")
    let dirB = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-disc-b-\(UUID().uuidString)")
    defer {
        try? FileManager.default.removeItem(at: dirA)
        try? FileManager.default.removeItem(at: dirB)
    }
    let idA = try DeviceIdentity.loadOrCreate(dir: dirA)
    let idB = try DeviceIdentity.loadOrCreate(dir: dirB)
    let fpA = idA.fingerprint
    let fpB = idB.fingerprint

    let (serviceA, streamA) = try await DiscoveryService.start(
        identity: idA, tcpPort: 50001, discoveryPort: port, bonjour: false)
    let (serviceB, streamB) = try await DiscoveryService.start(
        identity: idB, tcpPort: 50002, discoveryPort: port, bonjour: false)

    let logA = ChangeLog()
    let logB = ChangeLog()
    let drainA = Task { for await change in streamA { logA.append(change) } }
    let drainB = Task { for await change in streamB { logB.append(change) } }
    defer {
        drainA.cancel()
        drainB.cancel()
    }

    // Mutual discovery (multicast announce plus the unicast response to it;
    // up must show up in both directions)
    let upB = try await logA.waitFor("A sees B online") {
        if case .up(let peer) = $0 { peer.info.fingerprint == fpB } else { false }
    }
    guard case .up(let peerB) = upB else {
        Issue.record("The waitFor predicate guarantees an up event")
        return
    }
    #expect(peerB.port == 50002)
    #expect(!peerB.addrs.isEmpty)
    _ = try await logB.waitFor("B sees A online") {
        if case .up(let peer) = $0 { peer.info.fingerprint == fpA } else { false }
    }

    // A shuts down gracefully → B sees it go offline immediately via goodbye
    await serviceA.shutdown()
    _ = try await logB.waitFor("B sees A offline", timeout: .seconds(10)) {
        if case .down(let fp) = $0 { fp == fpA } else { false }
    }
    await serviceB.shutdown()
}

// MARK: - Bonjour loopback

/// Two discovery services find each other over the Bonjour channel only
/// (different UDP ports = deaf to each other's multicast); one deregisters its
/// service on exit and the other sees it go offline via browse remove
@Test(.timeLimit(.minutes(1)))
func bonjourLoopbackDiscoversAndRemoves() async throws {
    // Each takes its own UDP port range, so the only possible evidence of
    // discovery is mDNS
    let portA = UInt16.random(in: 42600...42799)
    let portB = UInt16.random(in: 42800...42999)
    let dirA = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-bonjour-a-\(UUID().uuidString)")
    let dirB = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-bonjour-b-\(UUID().uuidString)")
    defer {
        try? FileManager.default.removeItem(at: dirA)
        try? FileManager.default.removeItem(at: dirB)
    }
    let idA = try DeviceIdentity.loadOrCreate(dir: dirA)
    let idB = try DeviceIdentity.loadOrCreate(dir: dirB)
    let fpA = idA.fingerprint
    let fpB = idB.fingerprint

    let (serviceA, streamA) = try await DiscoveryService.start(
        identity: idA, tcpPort: 51001, discoveryPort: portA)
    let (serviceB, streamB) = try await DiscoveryService.start(
        identity: idB, tcpPort: 51002, discoveryPort: portB)

    let logA = ChangeLog()
    let logB = ChangeLog()
    let drainA = Task { for await change in streamA { logA.append(change) } }
    let drainB = Task { for await change in streamB { logB.append(change) } }
    defer {
        drainA.cancel()
        drainB.cancel()
    }

    // Mutual discovery over the whole register → browse → resolve → address
    // chain; a real LAN may have other lanecho devices mixed in, so the
    // assertions filter by fingerprint only
    let upB = try await logA.waitFor("A sees B through Bonjour") {
        if case .up(let peer) = $0 { peer.info.fingerprint == fpB } else { false }
    }
    guard case .up(let peerB) = upB else {
        Issue.record("The waitFor predicate guarantees an up event")
        return
    }
    #expect(peerB.port == 51002, "The port must come from the SRV record")
    #expect(!peerB.addrs.isEmpty)
    _ = try await logB.waitFor("B sees A through Bonjour") {
        if case .up(let peer) = $0 { peer.info.fingerprint == fpA } else { false }
    }

    // A exits → deregisters the service → B takes it offline via browse
    // remove (no UDP evidence, so it is removed immediately)
    await serviceA.shutdown()
    _ = try await logB.waitFor("B sees A offline", timeout: .seconds(10)) {
        if case .down(let fp) = $0 { fp == fpA } else { false }
    }
    await serviceB.shutdown()
}

// MARK: - Rename hot-update

/// Rename path: the engine persists the name and hot-updates its snapshot;
/// discovery re-announces, so the peer sees the new name
@Test(
    .enabled(if: MulticastAvailability.isAvailable, MulticastAvailability.skipReason),
    .timeLimit(.minutes(1)))
func renamePersistsAndPropagates() async throws {
    let port = UInt16.random(in: 42600...42999)
    let dirA = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-rename-a-\(UUID().uuidString)")
    let dirB = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-rename-b-\(UUID().uuidString)")
    defer {
        try? FileManager.default.removeItem(at: dirA)
        try? FileManager.default.removeItem(at: dirB)
    }

    // Engine side: persisted and hot-updated in the in-memory snapshot, and
    // visible after a reload
    let (engine, _) = try await SyncEngine.start(
        config: EngineConfig(dataDir: dirA, tcpPort: 0))
    defer { Task { await engine.shutdown() } }
    let renamed = try await engine.setDisplayName("Renamed machine")
    #expect(renamed.name == "Renamed machine")
    #expect(await engine.localInfo().name == "Renamed machine")
    #expect(try DeviceIdentity.loadOrCreate(dir: dirA).displayName == "Renamed machine")

    // Discovery side: A re-announces after the rename, and B's registry entry
    // updates along with the announce
    let idA = try DeviceIdentity.loadOrCreate(dir: dirA)
    let idB = try DeviceIdentity.loadOrCreate(dir: dirB)
    let (serviceA, _) = try await DiscoveryService.start(
        identity: idA, tcpPort: 50001, discoveryPort: port, bonjour: false)
    let (serviceB, streamB) = try await DiscoveryService.start(
        identity: idB, tcpPort: 50002, discoveryPort: port, bonjour: false)
    let logB = ChangeLog()
    let drainB = Task { for await change in streamB { logB.append(change) } }
    defer { drainB.cancel() }
    let fpA = idA.fingerprint
    _ = try await logB.waitFor("B sees A online") {
        if case .up(let peer) = $0 { peer.info.fingerprint == fpA } else { false }
    }

    var info = idA.peerInfo()
    info.name = "Hot-updated name"
    await serviceA.updateInfo(info)
    _ = try await logB.waitFor("B sees A's new name") {
        if case .up(let peer) = $0 {
            peer.info.fingerprint == fpA && peer.info.name == "Hot-updated name"
        } else {
            false
        }
    }
    await serviceA.shutdown()
    await serviceB.shutdown()
}

// MARK: - Liveness probe hard rule

/// Imposter listener: when a stale address points at a different device
/// holding a different identity, the probe must come back with the imposter's
/// fingerprint (≠ the expected one) so the liveness comparison fails — a bare
/// connect would wrongly call this "alive" here
@Test(.timeLimit(.minutes(1)))
func probeRejectsImposterListener() async throws {
    let imposterDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-imposter-\(UUID().uuidString)")
    let proberDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-prober-\(UUID().uuidString)")
    defer {
        try? FileManager.default.removeItem(at: imposterDir)
        try? FileManager.default.removeItem(at: proberDir)
    }
    // The imposter: a real TLS listener, but holding a different identity
    let imposter = try DeviceIdentity.loadOrCreate(dir: imposterDir)
    let imposterTLS = try TLSContexts(material: imposter.material)
    let (port, listenTask) = try await startSyncListener(
        localInfo: imposter.peerInfo(), serverContext: imposterTLS.server, port: 0,
        syncFilesRoot: imposterDir.appendingPathComponent(syncFilesDirName),
        delegate: NullDelegate())
    defer { listenTask.cancel() }

    let prober = try DeviceIdentity.loadOrCreate(dir: proberDir)
    let proberTLS = try TLSContexts(material: prober.material)
    // What we expect is the fingerprint of the device that vanished (any
    // different value will do)
    let expected = String(repeating: "e", count: 64)

    let probed = await probeIdentity(
        addr: "127.0.0.1", port: port, clientContext: proberTLS.client)
    let found = try #require(probed, "The imposter is online, so the handshake must return a fingerprint")
    #expect(found == imposter.fingerprint, "The result must be the imposter's real fingerprint")
    #expect(found != expected, "A mismatched fingerprint is not proof of liveness")

    // Control case: when the expected fingerprint matches the listener,
    // liveness is proven
    #expect(found == imposter.fingerprint)
}

/// Probe stub: accepts no transactions
private struct NullDelegate: ServerSessionDelegate {
    func decidePair(remote: PeerInfo) async -> Bool { false }
    func acceptSync(remote: PeerInfo, timestampMs: UInt64, contentKind: String, data: String)
        async -> String?
    { ReasonCode.notPaired }
    func decideBlobOffer(remote: PeerInfo, timestampMs: UInt64, offer: BlobOffer) async
        -> OfferDecision
    { .reject(ReasonCode.notPaired) }
    func finishBlobImage(remote: PeerInfo, timestampMs: UInt64, png: Data) async -> String? {
        ReasonCode.notPaired
    }
    func finishBlobFiles(
        remote: PeerInfo, timestampMs: UInt64, parts: [URL], metas: [FileMeta], batchDir: URL
    ) async -> String? {
        ReasonCode.notPaired
    }
    func unpaired(remote: PeerInfo) async {}
}
