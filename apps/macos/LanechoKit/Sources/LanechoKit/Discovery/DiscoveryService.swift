// Discovery service: the UDP channel plus the liveness probe, mirroring the
// Rust discovery.rs.
//
// Carries the UDP multicast channel (announce/response/goodbye + heartbeat +
// sweep + liveness probe); the mDNS/Bonjour channel (register + browse) feeds
// the same registry.
//
// The UDP channel uses NIO's DatagramBootstrap rather than Network.framework:
// NWConnectionGroup's multicast bind ignores allowLocalEndpointReuse, so a
// second instance on the same machine (or coexistence with the Tauri build)
// fails outright with EADDRINUSE. NIO can set SO_REUSEADDR + SO_REUSEPORT
// explicitly, matching the bind semantics of socket2 on the Rust side exactly.
//
// Liveness probe hard rule, never revert it: a liveness decision requires a
// TLS handshake and a certificate fingerprint comparison. A bare connect
// succeeding is not proof of life — a stale address may point at a different
// device that also listens on this port. On a fingerprint mismatch keep trying
// the next address; only when no address yields the real peer is it dead.

import Darwin
import Foundation
import NIOCore
import NIOPosix
import NIOSSL

/// Discovery service
public actor DiscoveryService {
    /// Registry
    /// A pending inbound packet (element of the receive queue)
    struct InboundPacket: Sendable {
        let packet: AnnouncePacket
        let source: SocketAddress
    }

    private var registry: PeerRegistry
    /// UDP multicast channel
    private let channel: Channel
    /// Multicast destination address
    private let multicastAddress: SocketAddress
    /// Pre-encoded packets, reused while the identity is unchanged; empty and
    /// never sent when passive, re-encoded as a batch by updateInfo on rename
    private var announceData: Data
    private var responseData: Data
    private var goodbyeData: Data
    /// The sync port we advertise (kept for re-encoding)
    private let tcpPort: UInt16
    /// Passive: receive only, never send
    private let passive: Bool
    /// Client TLS context for the liveness probe; no pinning, the comparison
    /// is explicit after the handshake
    private let probeContext: NIOSSLContext
    /// Outlet for change events
    private let changes: AsyncStream<RegistryChange>.Continuation
    /// Background tasks (heartbeat, sweep)
    private var tasks: [Task<Void, Never>] = []
    /// Bonjour channel; nil when disabled — isolated tests cannot use the real
    /// service type
    private var bonjourChannel: BonjourChannel?
    /// Entry point of the receive queue; finished on shutdown so the consuming
    /// task ends on its own
    private var packetSink: AsyncStream<InboundPacket>.Continuation?

    private init(
        registry: PeerRegistry, channel: Channel, multicastAddress: SocketAddress,
        announceData: Data, responseData: Data, goodbyeData: Data,
        tcpPort: UInt16, passive: Bool,
        probeContext: NIOSSLContext,
        changes: AsyncStream<RegistryChange>.Continuation
    ) {
        self.registry = registry
        self.channel = channel
        self.multicastAddress = multicastAddress
        self.announceData = announceData
        self.responseData = responseData
        self.goodbyeData = goodbyeData
        self.tcpPort = tcpPort
        self.passive = passive
        self.probeContext = probeContext
        self.changes = changes
    }

    /// Start discovery: bind the multicast port, join the multicast group, and
    /// begin receiving packets, heartbeating and sweeping.
    ///
    /// - `tcpPort`: the sync port advertised to peers
    /// - `discoveryPort`: multicast port; tests isolate on a custom port,
    ///   production uses 42525
    /// - `passive`: receive only, never send; Bonjour browses without
    ///   registering
    /// - `bonjour`: mDNS channel switch. The service type is global and cannot
    ///   be isolated per test, so pure-UDP tests must turn it off explicitly;
    ///   production always has it on
    public static func start(
        identity: DeviceIdentity,
        tcpPort: UInt16,
        discoveryPort: UInt16 = Config.discoveryPort,
        passive: Bool = false,
        bonjour: Bool = true
    ) async throws -> (DiscoveryService, AsyncStream<RegistryChange>) {
        let info = identity.peerInfo()
        let announce = passive
            ? Data()
            : try AnnouncePacket(kind: .announce, info: info, tcpPort: tcpPort).encoded()
        let response = passive
            ? Data()
            : try AnnouncePacket(kind: .response, info: info, tcpPort: tcpPort).encoded()
        let goodbye = passive
            ? Data()
            : try AnnouncePacket(kind: .goodbye, info: info, tcpPort: tcpPort).encoded()

        // The receive handler calls back into the service through a weak
        // bridge: the channel is created before the service, and the strong
        // reference cycle service → channel → handler → service has to be
        // broken
        let bridge = WeakServiceBridge()
        // Packets go into a queue consumed in order by a **single** task. One
        // Task per packet would decouple arrival order from processing order:
        // when a peer process restarts quickly, the old instance's goodbye
        // races the new instance's announce, and a goodbye processed last
        // knocks the freshly online instance straight back offline until the
        // next 5s heartbeat — with sync failing throughout
        let (packets, packetSink) = AsyncStream.makeStream(
            of: InboundPacket.self,
            bufferingPolicy: .bufferingOldest(Config.eventChannelCap))
        let bootstrap = DatagramBootstrap(group: MultiThreadedEventLoopGroup.singleton)
            .channelOption(ChannelOptions.socketOption(.so_reuseaddr), value: 1)
            .channelOption(
                ChannelOptions.socketOption(.init(rawValue: SO_REUSEPORT)), value: 1
            )
            .channelInitializer { channel in
                channel.pipeline.addHandler(
                    DiscoveryDatagramHandler { data, source in
                        guard let packet = AnnouncePacket.decoded(from: data) else { return }
                        packetSink.yield(InboundPacket(packet: packet, source: source))
                    })
            }
        let channel = try await bootstrap.bind(host: "0.0.0.0", port: Int(discoveryPort)).get()
        let multicastAddress: SocketAddress
        do {
            multicastAddress = try SocketAddress(
                ipAddress: Config.multicastGroup, port: Int(discoveryPort))
            guard let multicastChannel = channel as? MulticastChannel else {
                throw TransportError.peerUnreachable
            }
            try await multicastChannel.joinGroup(multicastAddress).get()
        } catch {
            // A failed join must release the bound port before rethrowing, so
            // no half-open channel leaks
            try? await channel.close().get()
            throw error
        }

        let (stream, continuation) = AsyncStream.makeStream(
            of: RegistryChange.self,
            bufferingPolicy: .bufferingNewest(Config.eventChannelCap))
        let probeContext = try TLSContexts(material: identity.material).client
        let service = DiscoveryService(
            registry: PeerRegistry(selfFingerprint: identity.fingerprint),
            channel: channel, multicastAddress: multicastAddress,
            announceData: announce, responseData: response, goodbyeData: goodbye,
            tcpPort: tcpPort, passive: passive,
            probeContext: probeContext, changes: continuation)
        bridge.set(service)

        if bonjour {
            // The mDNS and UDP channels feed the same registry, pointing back
            // at the service through the bridge
            do {
                let bonjourChannel = try BonjourChannel.start(
                    info: info, tcpPort: tcpPort, passive: passive,
                    onPeer: { peer in
                        Task { await bridge.get()?.inject(peer, source: .mdns) }
                    },
                    onRemoved: { deviceId in
                        Task { await bridge.get()?.mdnsRemoved(deviceId: deviceId) }
                    })
                await service.attachBonjour(bonjourChannel)
            } catch {
                // Same cleanup as the joinGroup failure above: a failed
                // register/browse must not leak the bound UDP port and the
                // event stream — a leaked port makes the next start fail to
                // bind
                try? await channel.close().get()
                continuation.finish()
                throw error
            }
        }

        await service.startTasks(passive: passive)
        await service.startPacketLoop(packets, sink: packetSink)
        return (service, stream)
    }

    /// Attach the Bonjour channel; its callbacks go through the weak bridge,
    /// so the service must already be registered in the bridge
    private func attachBonjour(_ channel: BonjourChannel) {
        bonjourChannel = channel
    }

    /// Heartbeat and sweep tasks
    /// Wire up the receive queue (one task, consumed in order; see the note in
    /// start)
    private func startPacketLoop(
        _ stream: AsyncStream<InboundPacket>,
        sink: AsyncStream<InboundPacket>.Continuation
    ) {
        packetSink = sink
        tasks.append(
            Task { [weak self] in
                for await inbound in stream {
                    await self?.handlePacket(inbound.packet, from: inbound.source)
                }
            })
    }

    private func startTasks(passive: Bool) {
        if !passive {
            tasks.append(
                Task { [weak self] in
                    while !Task.isCancelled {
                        await self?.sendAnnounce()
                        try? await Task.sleep(for: Config.heartbeatInterval)
                    }
                })
        }
        tasks.append(
            Task { [weak self] in
                while !Task.isCancelled {
                    // 2s granularity (Rust uses ≤5s), paired with the 15s
                    // timeout and the 30s probe interval. Do not make it more
                    // aggressive — transient Wi-Fi packet loss then reads as
                    // a peer going offline and the list flickers
                    try? await Task.sleep(for: .seconds(2))
                    await self?.sweepOnce()
                }
            })
    }

    /// Multicast one announce heartbeat
    private func sendAnnounce() {
        if !announceData.isEmpty {
            send(announceData, to: multicastAddress)
        }
    }

    /// Send one UDP packet, best effort, without waiting for completion
    private func send(_ data: Data, to destination: SocketAddress) {
        var buffer = channel.allocator.buffer(capacity: data.count)
        buffer.writeBytes(data)
        channel.writeAndFlush(
            AddressedEnvelope(remoteAddress: destination, data: buffer), promise: nil)
    }

    /// Handle one inbound discovery packet
    private func handlePacket(_ packet: AnnouncePacket, from source: SocketAddress) {
        switch packet.kind {
        case .announce, .response:
            guard let addr = source.ipAddress else { return }
            let peer = Peer(info: packet.info, addrs: [addr], port: packet.tcpPort)
            if let change = registry.upsert(peer, source: .udp, nowMs: nowMs()) {
                changes.yield(change)
            }
            // Unicast a response to an announce so a new peer sees the
            // existing ones at once. A response is never answered, to avoid
            // ping-pong; when passive it is empty and nothing is sent
            if case .announce = packet.kind, !responseData.isEmpty {
                send(responseData, to: source)
            }
        case .goodbye:
            if let change = registry.remove(fingerprint: packet.info.fingerprint) {
                changes.yield(change)
            }
        }
    }

    /// One sweep round: take timed-out peers offline and dispatch liveness
    /// probes
    private func sweepOnce() async {
        let (swept, probes) = registry.sweep(nowMs: nowMs())
        for change in swept {
            changes.yield(change)
        }
        for peer in probes {
            let context = probeContext
            Task { [weak self] in
                await self?.probe(peer: peer, context: context)
            }
        }
    }

    /// Probe one silent peer: a TLS handshake per address plus a fingerprint
    /// comparison (hard rule at the top of this file)
    private func probe(peer: Peer, context: NIOSSLContext) async {
        for addr in peer.addrs {
            if let fingerprint = await probeIdentity(
                addr: addr, port: peer.port, clientContext: context),
                fingerprint == peer.info.fingerprint
            {
                // Found the real peer: liveness established, this round is
                // done. A mismatch only means that address is stale
                return
            }
        }
        // No address yielded the real peer; if UDP revived within the probe
        // window, the newer evidence wins
        if let change = registry.probeFailed(fingerprint: peer.info.fingerprint, nowMs: nowMs()) {
            changes.yield(change)
        }
    }

    /// A peer came online or its info changed; the Bonjour channel feeds in
    /// here, and tests can inject directly
    public func inject(_ peer: Peer, source: PeerSource) {
        if let change = registry.upsert(peer, source: source, nowMs: nowMs()) {
            changes.yield(change)
        }
    }

    /// The mDNS service disappeared: clear the alive flag, and only go offline
    /// once UDP is silent too (semantics live in the registry)
    public func mdnsRemoved(deviceId: String) {
        if let change = registry.mdnsRemoved(deviceId: deviceId, nowMs: nowMs()) {
            changes.yield(change)
        }
    }

    /// Hot-update the device info on a rename: re-encode all three packet
    /// kinds, send one announce immediately so peers see the new name at once,
    /// and re-register the same Bonjour instance (TXT overwrite semantics)
    public func updateInfo(_ info: PeerInfo) {
        guard !passive else { return }
        do {
            announceData = try AnnouncePacket(kind: .announce, info: info, tcpPort: tcpPort)
                .encoded()
            responseData = try AnnouncePacket(kind: .response, info: info, tcpPort: tcpPort)
                .encoded()
            goodbyeData = try AnnouncePacket(kind: .goodbye, info: info, tcpPort: tcpPort)
                .encoded()
        } catch {
            // On an encoding failure keep the old packets; unreachable in
            // theory, PeerInfo always encodes
            return
        }
        send(announceData, to: multicastAddress)
        bonjourChannel?.updateRegistration(info: info, tcpPort: tcpPort)
    }

    /// Snapshot of the currently online peers
    public func peers() -> [Peer] {
        registry.snapshot()
    }

    /// Graceful shutdown: goodbye takes us offline on peers immediately, then
    /// stop the tasks and close both channels. Deregistering Bonjour makes the
    /// system broadcast the service going away, which peers see as a browse
    /// remove.
    public func shutdown() async {
        if !goodbyeData.isEmpty {
            send(goodbyeData, to: multicastAddress)
            // Give the send one beat; UDP has no acknowledgement, best effort
            try? await Task.sleep(for: .milliseconds(100))
        }
        packetSink?.finish()
        packetSink = nil
        for task in tasks {
            task.cancel()
        }
        bonjourChannel?.stop()
        bonjourChannel = nil
        try? await channel.close().get()
        changes.finish()
    }
}

/// Probe one address for the peer's identity: a TLS handshake to read the
/// certificate fingerprint, then disconnect.
///
/// No pinning — every peer expects a different fingerprint — so the liveness
/// decision rests on the caller's explicit comparison. Returns nil if the
/// connection fails, the handshake fails, or it times out.
public func probeIdentity(
    addr: String, port: UInt16, clientContext: NIOSSLContext
) async -> String? {
    let box = FingerprintBox()
    guard
        let channel = try? await dialPeer(
            addrs: [addr], port: port, clientContext: clientContext,
            expectedFingerprint: nil, onServerFingerprint: { box.set($0) })
    else { return nil }
    // NIOSSL handshakes as soon as the connection is up; wait for the
    // fingerprint to land in the box, or for the timeout. The channel must be
    // consumed through executeThenClose — a bare close deinitializes the
    // NIOAsyncChannel writer before it is finished, which is fatal
    return try? await channel.executeThenClose { _, _ -> String? in
        let deadline = ContinuousClock.now + Config.peerProbeTimeout
        while ContinuousClock.now < deadline {
            if let fingerprint = box.get() {
                return fingerprint
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        return nil
    } ?? nil
}

/// UDP receive handler: hands (packet, source address) to the callback
private final class DiscoveryDatagramHandler: ChannelInboundHandler, @unchecked Sendable {
    typealias InboundIn = AddressedEnvelope<ByteBuffer>

    /// Receive callback
    private let onPacket: @Sendable (Data, SocketAddress) -> Void

    init(onPacket: @escaping @Sendable (Data, SocketAddress) -> Void) {
        self.onPacket = onPacket
    }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        let envelope = unwrapInboundIn(data)
        onPacket(Data(envelope.data.readableBytesView), envelope.remoteAddress)
    }

    func errorCaught(context: ChannelHandlerContext, error: Error) {
        // Occasional UDP errors (e.g. ICMP port unreachable) do not tear the
        // channel down
    }
}

/// Weak bridge: the channel is created before the service, so the handler
/// points back at the service through it, which also breaks the strong
/// reference cycle service → channel → handler → service
private final class WeakServiceBridge: @unchecked Sendable {
    private let lock = NSLock()
    private weak var service: DiscoveryService?

    func set(_ service: DiscoveryService) {
        lock.withLock { self.service = service }
    }

    func get() -> DiscoveryService? {
        lock.withLock { service }
    }
}
