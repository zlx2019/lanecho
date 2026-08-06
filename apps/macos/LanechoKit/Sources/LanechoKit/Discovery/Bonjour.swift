// Bonjour channel, mirroring the mDNS channel in the Rust discovery.rs.
//
// Unlike the Rust side (mdns-sd ships its own responder), this talks to the
// system mDNSResponder directly through the dnssd C API: register
// (DNSServiceRegister) + browse (DNSServiceBrowse) + resolve
// (DNSServiceResolve, kept alive to follow TXT changes) + address lookup
// (DNSServiceGetAddrInfo, kept alive to follow address changes).
//
// Deliberate deviation from the spec: the Rust build registers a custom host
// `<device_id>.local.` because mdns-sd, being a second responder, must not
// conflict with the system hostname records. This implementation registers
// through the system responder, so the host has to be the default hostname —
// whose A/AAAA records the system maintains — for it to resolve at all. Peers
// only read the SRV target and the TXT record, so interop is unaffected.
//
// Threading: every DNSServiceRef is bound to the same serial queue via
// DNSServiceSetDispatchQueue and mutable state is only touched on that queue;
// stop() dispatches synchronously onto the queue and deallocates every ref —
// after that the system guarantees no further callbacks, so no lock is needed.

import Foundation
import dnssd

/// Bonjour service type (regtype form, without the .local. suffix)
private let bonjourRegtype = "_lanecho._tcp"

/// Bonjour channel: registers this machine's service and browses for peers
///
/// Lifetime: it keeps working after `start` returns and must be stopped
/// explicitly via `stop()`. The owner is responsible; there is no deinit
/// fallback, because deallocating the C refs has to happen serially on the
/// callback queue.
final class BonjourChannel: @unchecked Sendable {
    /// Callback queue, shared by every ref; serialized to keep state coherent
    private let queue = DispatchQueue(label: "io.github.zlx2019.lanecho.bonjour")
    /// Our own registration (nil when passive)
    private var registerRef: DNSServiceRef?
    /// Browse
    private var browseRef: DNSServiceRef?
    /// Instance name → long-lived resolve session
    private var sessions: [String: ResolveSession] = [:]
    /// Our own instance name, skipped in browse results; the registry's
    /// fingerprint filter is the fallback
    private let selfInstance: String
    /// A usable peer was resolved; may fire repeatedly for multiple addresses
    /// or TXT changes, and the registry merges them
    private let onPeer: @Sendable (Peer) -> Void
    /// The service disappeared; the parameter is the instance name = device_id
    private let onRemoved: @Sendable (String) -> Void

    private init(
        selfInstance: String,
        onPeer: @escaping @Sendable (Peer) -> Void,
        onRemoved: @escaping @Sendable (String) -> Void
    ) {
        self.selfInstance = selfInstance
        self.onPeer = onPeer
        self.onRemoved = onRemoved
    }

    /// Long-lived resolve session: created as soon as the service is browsed,
    /// torn down only when it disappears
    ///
    /// The resolve stays up to follow TXT changes (a rename re-registers the
    /// same instance, which overwrites). Every resolve callback rebuilds the
    /// long-lived addrInfo lookup, because the host may have changed.
    private final class ResolveSession {
        var resolveRef: DNSServiceRef?
        var addrInfoRef: DNSServiceRef?
        /// Peer info parsed from TXT; missing fields mean this is not a
        /// lanecho service, so it stays nil
        var info: PeerInfo?
        /// Sync port carried by the SRV record
        var port: UInt16 = 0
        /// Back-pointer context: C callbacks come back through an Unmanaged
        /// raw pointer, so it is kept alive for the session's lifetime
        var context: InstanceContext?
    }

    /// C callback context: the channel plus the instance name
    final class InstanceContext {
        unowned let channel: BonjourChannel
        let instance: String

        init(channel: BonjourChannel, instance: String) {
            self.channel = channel
            self.instance = instance
        }
    }

    /// Start the channel: register our service (skipped when passive) and
    /// begin browsing
    static func start(
        info: PeerInfo, tcpPort: UInt16, passive: Bool,
        onPeer: @escaping @Sendable (Peer) -> Void,
        onRemoved: @escaping @Sendable (String) -> Void
    ) throws -> BonjourChannel {
        let channel = BonjourChannel(
            selfInstance: info.deviceId, onPeer: onPeer, onRemoved: onRemoved)
        try channel.queue.sync {
            if !passive {
                try channel.register(info: info, tcpPort: tcpPort)
            }
            try channel.browse()
        }
        return channel
    }

    /// Hot-update the info on a rename: deregister, then re-register the same
    /// instance. The instance is the device_id and does not follow the display
    /// name, so peers pick the new name up from TXT; the brief gap is absorbed
    /// by the fresh-UDP retention rule in the peer's registry, so nothing
    /// flickers offline.
    func updateRegistration(info: PeerInfo, tcpPort: UInt16) {
        queue.sync {
            if let ref = registerRef {
                DNSServiceRefDeallocate(ref)
                registerRef = nil
            }
            try? register(info: info, tcpPort: tcpPort)
        }
    }

    /// Stop the channel: deallocate every ref, after which the system
    /// broadcasts the service going away
    func stop() {
        queue.sync {
            for session in sessions.values {
                tearDown(session)
            }
            sessions.removeAll()
            if let ref = browseRef {
                DNSServiceRefDeallocate(ref)
                browseRef = nil
            }
            if let ref = registerRef {
                DNSServiceRefDeallocate(ref)
                registerRef = nil
            }
        }
    }

    // MARK: - Register

    /// Register our service: instance = device_id (unique, and does not follow
    /// the display name), plus five TXT fields
    private func register(info: PeerInfo, tcpPort: UInt16) throws {
        let txt = Self.makeTXT(info: info)
        var ref: DNSServiceRef?
        let err = txt.withUnsafeBufferPointer { buffer in
            DNSServiceRegister(
                &ref, 0, 0, info.deviceId, bonjourRegtype, nil, nil,
                tcpPort.bigEndian, UInt16(buffer.count), buffer.baseAddress,
                nil, nil)
        }
        guard err == kDNSServiceErr_NoError, let ref else {
            throw TransportError.timeout("bonjour register: \(err)")
        }
        DNSServiceSetDispatchQueue(ref, queue)
        registerRef = ref
    }

    /// Build the TXT record bytes; id/name/fp/platform are required, osv is
    /// optional
    private static func makeTXT(info: PeerInfo) -> [UInt8] {
        var txt = TXTRecordRef()
        TXTRecordCreate(&txt, 0, nil)
        defer { TXTRecordDeallocate(&txt) }
        func set(_ key: String, _ value: String) {
            let bytes = Array(value.utf8.prefix(255))
            _ = bytes.withUnsafeBufferPointer {
                TXTRecordSetValue(&txt, key, UInt8(bytes.count), $0.baseAddress)
            }
        }
        set("id", info.deviceId)
        set("name", info.name)
        set("fp", info.fingerprint)
        set("platform", info.platform)
        if let osv = info.osVersion {
            set("osv", osv)
        }
        let length = Int(TXTRecordGetLength(&txt))
        guard let bytes = TXTRecordGetBytesPtr(&txt) else { return [] }
        return Array(UnsafeRawBufferPointer(start: bytes, count: length))
    }

    // MARK: - Browse

    /// Browse services of the same type: add creates a long-lived resolve
    /// session, remove tears it down and reports it
    private func browse() throws {
        var ref: DNSServiceRef?
        let context = Unmanaged.passUnretained(self).toOpaque()
        let err = DNSServiceBrowse(
            &ref, 0, 0, bonjourRegtype, nil,
            { _, flags, _, errorCode, serviceName, _, _, context in
                guard errorCode == kDNSServiceErr_NoError,
                    let serviceName, let context
                else { return }
                let channel = Unmanaged<BonjourChannel>.fromOpaque(context)
                    .takeUnretainedValue()
                let instance = String(cString: serviceName)
                if flags & DNSServiceFlags(kDNSServiceFlagsAdd) != 0 {
                    channel.serviceAppeared(instance: instance)
                } else {
                    channel.serviceRemoved(instance: instance)
                }
            }, context)
        guard err == kDNSServiceErr_NoError, let ref else {
            throw TransportError.timeout("bonjour browse: \(err)")
        }
        DNSServiceSetDispatchQueue(ref, queue)
        browseRef = ref
    }

    /// A new service was browsed (on the queue): skip ourselves, set up the
    /// long-lived resolve
    private func serviceAppeared(instance: String) {
        guard instance != selfInstance, sessions[instance] == nil else { return }
        let session = ResolveSession()
        let context = InstanceContext(channel: self, instance: instance)
        session.context = context
        var ref: DNSServiceRef?
        let err = DNSServiceResolve(
            &ref, 0, 0, instance, bonjourRegtype, "local.",
            { _, _, _, errorCode, _, hosttarget, port, txtLen, txtRecord, context in
                guard errorCode == kDNSServiceErr_NoError,
                    let hosttarget, let context
                else { return }
                let box = Unmanaged<InstanceContext>.fromOpaque(context)
                    .takeUnretainedValue()
                box.channel.serviceResolved(
                    instance: box.instance,
                    host: String(cString: hosttarget),
                    port: UInt16(bigEndian: port),
                    txtLen: txtLen, txtRecord: txtRecord.map { UnsafeRawPointer($0) })
            }, Unmanaged.passUnretained(context).toOpaque())
        guard err == kDNSServiceErr_NoError, let ref else { return }
        DNSServiceSetDispatchQueue(ref, queue)
        session.resolveRef = ref
        sessions[instance] = session
    }

    /// A service disappeared (on the queue): tear the session down and report
    /// the device_id (= instance)
    private func serviceRemoved(instance: String) {
        guard let session = sessions.removeValue(forKey: instance) else { return }
        tearDown(session)
        onRemoved(instance)
    }

    /// A resolve result arrived (on the queue): record TXT and port, rebuild
    /// the address lookup session
    private func serviceResolved(
        instance: String, host: String, port: UInt16,
        txtLen: UInt16, txtRecord: UnsafeRawPointer?
    ) {
        guard let session = sessions[instance] else { return }
        // Missing fields mean this is not a lanecho service: emit no peer, but
        // keep the session — a TXT update can still qualify it
        guard let deviceId = Self.txtValue("id", txtLen, txtRecord),
            let name = Self.txtValue("name", txtLen, txtRecord),
            let fingerprint = Self.txtValue("fp", txtLen, txtRecord),
            let platform = Self.txtValue("platform", txtLen, txtRecord)
        else {
            session.info = nil
            return
        }
        session.info = PeerInfo(
            deviceId: deviceId, name: name, fingerprint: fingerprint,
            platform: platform, osVersion: Self.txtValue("osv", txtLen, txtRecord))
        session.port = port
        // The host can change between resolves, so rebuild the whole address
        // lookup session
        if let ref = session.addrInfoRef {
            DNSServiceRefDeallocate(ref)
            session.addrInfoRef = nil
        }
        guard let context = session.context else { return }
        var ref: DNSServiceRef?
        let err = DNSServiceGetAddrInfo(
            &ref, 0, 0,
            DNSServiceProtocol(kDNSServiceProtocol_IPv4 | kDNSServiceProtocol_IPv6),
            host,
            { _, flags, _, errorCode, _, address, _, context in
                guard errorCode == kDNSServiceErr_NoError,
                    flags & DNSServiceFlags(kDNSServiceFlagsAdd) != 0,
                    let address, let context
                else { return }
                let box = Unmanaged<InstanceContext>.fromOpaque(context)
                    .takeUnretainedValue()
                if let addr = ipString(of: address) {
                    box.channel.addressFound(instance: box.instance, addr: addr)
                }
            }, Unmanaged.passUnretained(context).toOpaque())
        guard err == kDNSServiceErr_NoError, let ref else { return }
        DNSServiceSetDispatchQueue(ref, queue)
        session.addrInfoRef = ref
    }

    /// One address arrived (on the queue): emit a peer per address and let the
    /// registry merge them (addresses only ever accumulate)
    private func addressFound(instance: String, addr: String) {
        guard let session = sessions[instance], let info = session.info else { return }
        onPeer(Peer(info: info, addrs: [addr], port: session.port))
    }

    /// Deallocate every ref of one session
    private func tearDown(_ session: ResolveSession) {
        if let ref = session.addrInfoRef {
            DNSServiceRefDeallocate(ref)
            session.addrInfoRef = nil
        }
        if let ref = session.resolveRef {
            DNSServiceRefDeallocate(ref)
            session.resolveRef = nil
        }
        session.context = nil
    }

    /// Read one TXT value; nil when absent
    private static func txtValue(
        _ key: String, _ txtLen: UInt16, _ txtRecord: UnsafeRawPointer?
    ) -> String? {
        var valueLen: UInt8 = 0
        guard let ptr = TXTRecordGetValuePtr(txtLen, txtRecord, key, &valueLen) else {
            return nil
        }
        return String(
            decoding: UnsafeRawBufferPointer(start: ptr, count: Int(valueLen)), as: UTF8.self)
    }
}

/// sockaddr → numeric address string; link-local v6 carries a %scope and is
/// filtered out by normalizeAddrs
private func ipString(of address: UnsafePointer<sockaddr>) -> String? {
    var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
    let ok = getnameinfo(
        address, socklen_t(address.pointee.sa_len),
        &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST)
    guard ok == 0 else { return nil }
    let bytes = host.prefix(while: { $0 != 0 }).map(UInt8.init(bitPattern:))
    return String(decoding: bytes, as: UTF8.self)
}
