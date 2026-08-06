// Peer registry, mirroring Registry in the Rust discovery.rs: merges the mDNS
// and UDP sources and maintains online state. The liveness hard rules land
// here.
//
// - Addresses only accumulate: stale ones are never pruned individually, they
//   are rebuilt after the peer goes offline as a whole
// - Liveness is tracked per source: mDNS can only be declared dead by
//   ServiceRemoved (it has no periodic heartbeat, so it cannot time out); UDP
//   is dead once the timeout window passes with no heartbeat; a peer goes
//   offline only when both channels are dead
// - Peers that are alive on mDNS only, with UDP silent, are handed to the
//   liveness probe (TLS handshake + fingerprint comparison) to decide
//
// Pure logic — the caller injects time; the network wiring lives in
// DiscoveryService.

import Foundation

/// Source channel a peer's info came from
public enum PeerSource: Sendable, Equatable {
    /// mDNS browse: event driven, producing no repeat events while the service
    /// is stable
    case mdns
    /// UDP multicast: heartbeat driven, refreshed every heartbeat period
    case udp
}

/// State change produced by the registry; the caller forwards it to the engine
public enum RegistryChange: Sendable, Equatable {
    /// A peer came online or its info changed
    case up(Peer)
    /// A peer went offline; the parameter is its fingerprint
    case down(String)
}

/// Peer registry: value semantics plus injected time, so the timing rules can
/// be tested exactly
public struct PeerRegistry: Sendable {
    /// Peer state
    private struct State {
        var peer: Peer
        /// Last UDP heartbeat (milliseconds); nil if never seen over UDP
        var lastUdpMs: UInt64?
        /// Alive on the mDNS channel
        var mdnsAlive: Bool
        /// When the last liveness probe was started (throttling); coming
        /// online counts as just probed
        var lastProbeMs: UInt64?
    }

    /// Our own fingerprint, used to filter out seeing ourselves
    private let selfFingerprint: String
    /// Fingerprint → state
    private var peers: [String: State] = [:]

    /// Construct with our own fingerprint
    public init(selfFingerprint: String) {
        self.selfFingerprint = selfFingerprint
    }

    /// Add or refresh a peer; only an actual info change produces an up, a
    /// heartbeat merely refreshes the timestamp.
    ///
    /// Address merge: the two channels complement each other, so addresses are
    /// merged, deduplicated and normalized (IPv4 first, stable sort); an
    /// update left with no usable address after normalizing is dropped. A new
    /// source only strengthens liveness, it never clears the other one.
    public mutating func upsert(_ incoming: Peer, source: PeerSource, nowMs: UInt64)
        -> RegistryChange?
    {
        guard incoming.info.fingerprint != selfFingerprint else { return nil }
        var peer = incoming
        let fingerprint = peer.info.fingerprint
        let changed: Bool
        var lastUdpMs: UInt64?
        var mdnsAlive = false
        var lastProbeMs: UInt64?
        if let state = peers[fingerprint] {
            var merged = state.peer.addrs
            for addr in peer.addrs where !merged.contains(addr) {
                merged.append(addr)
            }
            peer.addrs = normalizeAddrs(merged)
            changed = state.peer != peer
            lastUdpMs = state.lastUdpMs
            mdnsAlive = state.mdnsAlive
            lastProbeMs = state.lastProbeMs
        } else {
            peer.addrs = normalizeAddrs(peer.addrs)
            changed = true
            // Coming online is itself proof of life, so treat it as just
            // probed: the first probe waits out a full interval
            lastProbeMs = nowMs
        }
        guard !peer.addrs.isEmpty else { return nil }
        switch source {
        case .udp: lastUdpMs = nowMs
        case .mdns: mdnsAlive = true
        }
        peers[fingerprint] = State(
            peer: peer, lastUdpMs: lastUdpMs, mdnsAlive: mdnsAlive, lastProbeMs: lastProbeMs)
        return changed ? .up(peer) : nil
    }

    /// Remove a peer by fingerprint
    public mutating func remove(fingerprint: String) -> RegistryChange? {
        peers.removeValue(forKey: fingerprint) != nil ? .down(fingerprint) : nil
    }

    /// The mDNS service disappeared (goodbye or TTL expiry): clear the mDNS
    /// alive flag. The peer is kept while its UDP heartbeat is still fresh —
    /// degrading to a single channel must not flicker it offline — otherwise
    /// it is removed immediately.
    public mutating func mdnsRemoved(deviceId: String, nowMs: UInt64) -> RegistryChange? {
        guard let (fingerprint, state) = peers.first(where: { $1.peer.info.deviceId == deviceId })
        else { return nil }
        var updated = state
        updated.mdnsAlive = false
        peers[fingerprint] = updated
        let udpDead = udpSilent(state.lastUdpMs, nowMs: nowMs)
        return udpDead ? remove(fingerprint: fingerprint) : nil
    }

    /// Drop dead peers and return the suspect peers that need a TCP liveness
    /// probe.
    ///
    /// - Cleanup: not alive on mDNS and UDP timed out (or never arrived) →
    ///   removed. Peers alive on mDNS are **never dropped on time alone**;
    ///   their going offline is driven by ServiceRemoved or the liveness probe
    /// - Probe: peers alive on mDNS only, with UDP silent, are handed to the
    ///   caller to connect to, stamping the probe time on the way out
    ///   (throttled by peerProbeInterval); being returned means "due"
    public mutating func sweep(nowMs: UInt64) -> (changes: [RegistryChange], probes: [Peer]) {
        var probes: [Peer] = []
        for (fingerprint, var state) in peers {
            let probeDue =
                state.lastProbeMs.map {
                    nowMs &- $0 > UInt64(Config.peerProbeInterval.components.seconds * 1000)
                } ?? true
            if state.mdnsAlive, udpSilent(state.lastUdpMs, nowMs: nowMs), probeDue {
                state.lastProbeMs = nowMs
                peers[fingerprint] = state
                probes.append(state.peer)
            }
        }
        let expired = peers.filter { !$1.mdnsAlive && udpSilent($1.lastUdpMs, nowMs: nowMs) }
            .map(\.key)
        var changes: [RegistryChange] = []
        for fingerprint in expired {
            if let change = remove(fingerprint: fingerprint) {
                changes.append(change)
            }
        }
        return (changes, probes)
    }

    /// Whether a peer's UDP heartbeat is still inside the window; this newer
    /// evidence overrules the death sentence of a failed liveness probe
    public func udpFresh(fingerprint: String, nowMs: UInt64) -> Bool {
        guard let state = peers[fingerprint] else { return false }
        return !udpSilent(state.lastUdpMs, nowMs: nowMs)
    }

    /// Apply a failed liveness probe: if UDP has revived, the newer evidence
    /// wins and the peer is kept; otherwise clear the mDNS flag and remove it.
    /// The probe's verdict only refutes liveness that mDNS alone attested.
    public mutating func probeFailed(fingerprint: String, nowMs: UInt64) -> RegistryChange? {
        guard let state = peers[fingerprint] else { return nil }
        if !udpSilent(state.lastUdpMs, nowMs: nowMs) {
            return nil
        }
        var updated = state
        updated.mdnsAlive = false
        peers[fingerprint] = updated
        return remove(fingerprint: fingerprint)
    }

    /// Snapshot of the currently online peers
    public func snapshot() -> [Peer] {
        peers.values.map(\.peer)
    }

    /// Whether UDP is silent: no heartbeat within the timeout window, or none
    /// ever
    private func udpSilent(_ lastUdpMs: UInt64?, nowMs: UInt64) -> Bool {
        guard let last = lastUdpMs else { return true }
        return nowMs &- last > UInt64(Config.peerTimeout.components.seconds * 1000)
    }
}

/// Normalize candidate addresses: drop IPv6 link-local addresses that cannot
/// be dialed directly (fe80::/10, no scope id), and rank non-loopback IPv4
/// first (stable sort, so equal ranks keep arrival order)
public func normalizeAddrs(_ all: [String]) -> [String] {
    let usable = all.filter { !$0.lowercased().hasPrefix("fe80:") }
    // Stable sort key: (is loopback, is not IPv4, arrival order)
    func rank(_ addr: String) -> Int {
        (isLoopback(addr) ? 2 : 0) + (isIPv4(addr) ? 0 : 1)
    }
    return usable.enumerated()
        .sorted { lhs, rhs in
            let lk = rank(lhs.element)
            let rk = rank(rhs.element)
            return lk == rk ? lhs.offset < rhs.offset : lk < rk
        }
        .map(\.element)
}

/// Whether the address is a loopback address
private func isLoopback(_ addr: String) -> Bool {
    addr == "127.0.0.1" || addr == "::1" || addr.hasPrefix("127.")
}

/// Whether the address is an IPv4 literal
private func isIPv4(_ addr: String) -> Bool {
    let parts = addr.split(separator: ".")
    return parts.count == 4 && parts.allSatisfy { UInt8($0) != nil }
}
