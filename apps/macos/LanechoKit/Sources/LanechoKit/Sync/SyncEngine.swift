// Sync engine (mirroring the Rust sync/mod.rs): pairing model + in-group
// broadcast + LWW + echo suppression
//
// The engine never touches the system clipboard itself:
// - Its input is a ClipboardEvent (in production the shell layer wires up the
//   clipboard watcher; tests inject events directly)
// - Remote syncs land through EngineEvent.applyRemote, which the shell layer
//   writes; the echo hash is registered **before** the event is emitted (the
//   write happens before the watcher's next poll, so the order holds), and a
//   failed write in the shell layer must call cancelEcho — otherwise the orphan
//   hash swallows the next genuine copy of the same content
// - The peer directory is driven by the discovery layer through
//   peerUp/peerDown (tests inject directly)
//
// Echo suppression is a hard rule: content written by a remote is never
// broadcast; picking an entry from the history panel is explicit user intent,
// goes through the normal copy path and broadcasts as usual.

import Foundation

/// A peer as the discovery layer sees it (info plus candidate addresses)
public struct Peer: Sendable, Equatable {
    /// Device info
    public var info: PeerInfo
    /// Candidate addresses (several network interfaces; the discovery layer
    /// maintains the append-only semantics)
    public var addrs: [String]
    /// TCP listening port
    public var port: UInt16

    /// Field-by-field initializer
    public init(info: PeerInfo, addrs: [String], port: UInt16) {
        self.info = info
        self.addrs = addrs
        self.port = port
    }
}

/// Sync direction policy: send and receive are gated independently
///
/// rawValue is exactly what settings.json stores; falling back to `both` on an
/// unknown string is the shell layer's job.
public enum SyncMode: String, Sendable, Equatable {
    /// Off entirely: a purely local clipboard history
    case off
    /// Sync both ways (the default)
    case both
    /// Send only: broadcast local copies, refuse remote syncs
    case send
    /// Receive only: never broadcast, only take remote syncs
    case receive

    /// Whether local copies may be broadcast
    public var sends: Bool { self == .both || self == .send }
    /// Whether remote syncs are accepted
    public var receives: Bool { self == .both || self == .receive }
}

/// Sync type toggles: only the checked types are sent and received
public struct SyncTypes: Sendable, Equatable {
    /// Text
    public var text: Bool
    /// Images
    public var images: Bool
    /// Files
    public var files: Bool

    /// Files involve writing to disk and cleaning it up, so they default to off
    public init(text: Bool = true, images: Bool = true, files: Bool = false) {
        self.text = text
        self.images = images
        self.files = files
    }
}

/// Engine configuration
public struct EngineConfig: Sendable {
    /// Data directory (where the identity and pairing files live)
    public var dataDir: URL
    /// TCP listening port (0 = assigned at random, for tests and several
    /// instances on one machine)
    public var tcpPort: UInt16
    /// Initial sync direction policy (switchable at runtime)
    public var syncMode: SyncMode
    /// Initial sync type toggles (one toggle governs both directions;
    /// switchable at runtime)
    public var syncTypes: SyncTypes
    /// Cap on the total bytes of a file sync (switchable at runtime)
    public var maxSyncFileBytes: UInt64

    /// Field-by-field initializer
    public init(
        dataDir: URL, tcpPort: UInt16 = Config.tcpPort, syncMode: SyncMode = .both,
        syncTypes: SyncTypes = SyncTypes(), maxSyncFileBytes: UInt64 = 32 * 1024 * 1024
    ) {
        self.dataDir = dataDir
        self.tcpPort = tcpPort
        self.syncMode = syncMode
        self.syncTypes = syncTypes
        self.maxSyncFileBytes = maxSyncFileBytes
    }
}

/// Engine events: the UI's only source of truth
public enum EngineEvent: Sendable {
    /// A peer came online, or its info changed
    case peerUp(Peer)
    /// A peer went offline (the parameter is its fingerprint)
    case peerDown(String)
    /// An inbound pairing request arrived; the layer above prompts the user and
    /// feeds the decision back through respondPair
    case pairRequested(PeerInfo)
    /// A pairing is in place (both ways: a successful outbound pairing and an
    /// accepted inbound one both fire this)
    case paired(PeerInfo)
    /// A pairing was removed (locally, or on notice from the peer)
    case unpaired(fingerprint: String)
    /// The local user copied something new (the history pipeline and the UI
    /// hints hang off this event); suppressRecord is the ignore-rule verdict
    /// riding through from the shell (see ClipboardEvent)
    case localCopied(
        content: ClipboardContent, hash: String, timestampMs: UInt64, suppressRecord: Bool)
    /// A remote sync was accepted; the shell layer should write it to the
    /// system clipboard (contract in the file header)
    ///
    /// Since 1.1 content covers all three kinds: text byte-for-byte as it came;
    /// images as decoded RGBA (the echo hash is the RGBA hash, the same
    /// baseline history dedup uses); files as the **local landing paths**
    /// (under sync-files, with the echo hash computed over those paths — the
    /// peer's paths mean nothing here).
    case applyRemote(content: ClipboardContent, from: PeerInfo, timestampMs: UInt64, hash: String)
    /// The outcome of one outbound sync, reported per target peer (a nil error
    /// means it was delivered)
    case syncSent(to: PeerInfo, error: String?)
}

/// A pending pairing request: the decision channel plus a generation number (a
/// concurrent supersede only cleans up its own generation)
private struct PendingPair {
    /// Request generation (monotonically increasing)
    let generation: UInt64
    /// Device info of the requester
    let peer: PeerInfo
    /// The decision continuation (resumed by exactly one of respondPair, the
    /// timeout, or a supersede; set to nil the moment it resumes)
    var continuation: CheckedContinuation<Bool, Never>?
    /// Fallback task for the decision timeout
    ///
    /// All three resume paths (answered, superseded, expired) must cancel it —
    /// otherwise it keeps sleeping out the full 300 seconds after the user hit
    /// "Accept", holding a strong reference to the whole engine the whole time.
    var timeout: Task<Void, Never>?
}

/// The sync engine
public actor SyncEngine {
    /// Local identity (a display-name update swaps the whole snapshot; the
    /// fingerprint does not change)
    private var identity: DeviceIdentity
    /// TLS context pair (a rename does not change the certificate, so there is
    /// nothing to rebuild)
    private let contexts: TLSContexts
    /// Data directory (used to persist a rename)
    private let dataDir: URL
    /// The pairing set
    private var paired: PairedStore
    /// Hashes of the most recent remote writes (echo registrations, consumed
    /// once; ring of 8)
    ///
    /// Each carries its registration time: the only consumer is an event from
    /// the watcher, and when the written content equals what is already on the
    /// clipboard the watcher produces no event — that registration is left an
    /// orphan. See [`Config.echoTTL`].
    private var echo: [(hash: String, atMs: UInt64)] = []
    /// Timestamp of the last local copy (Unix milliseconds), the LWW tiebreak
    /// baseline
    private var lastLocalCopyMs: UInt64 = 0
    /// Direction gate: send (whether local copies are broadcast)
    private var sendEnabled: Bool
    /// Direction gate: receive (whether remote syncs are accepted)
    private var recvEnabled: Bool
    /// Type gates (shared by send and receive; one toggle governs both
    /// directions)
    private var types: SyncTypes
    /// Cap on the total bytes of a file sync
    private var maxSyncFileBytes: UInt64
    /// Inbound pairing requests waiting on a UI decision (fingerprint → pending
    /// record)
    private var pendingPairs: [String: PendingPair] = [:]
    /// Generation counter for pairing requests
    private var pendingSeq: UInt64 = 0
    /// Monotonic sequence number for outbound syncs (for log forensics)
    private var seq: UInt64 = 0
    /// Directory of online peers (driven by the discovery layer; keyed by
    /// fingerprint)
    private var peersByFingerprint: [String: Peer] = [:]
    /// Outlet for engine events
    private let events: AsyncStream<EngineEvent>.Continuation
    /// The port actually bound (filled in once start finishes)
    private var boundPort: UInt16 = 0
    /// The accept listener task (cancelled on shutdown)
    private var listenerTask: Task<Void, Never>?

    private init(
        identity: DeviceIdentity, contexts: TLSContexts, config: EngineConfig,
        events: AsyncStream<EngineEvent>.Continuation
    ) {
        self.identity = identity
        self.contexts = contexts
        self.dataDir = config.dataDir
        self.paired = PairedStore(dir: config.dataDir)
        self.sendEnabled = config.syncMode.sends
        self.recvEnabled = config.syncMode.receives
        self.types = config.syncTypes
        self.maxSyncFileBytes = config.maxSyncFileBytes
        self.events = events
    }

    /// Start the engine: load the identity, bind the listener, wire up the
    /// accept delegate
    public static func start(config: EngineConfig) async throws -> (
        SyncEngine, AsyncStream<EngineEvent>
    ) {
        let identity = try DeviceIdentity.loadOrCreate(dir: config.dataDir)
        let contexts = try TLSContexts(material: identity.material)
        let (stream, continuation) = AsyncStream.makeStream(
            of: EngineEvent.self,
            bufferingPolicy: .bufferingNewest(Config.eventChannelCap))
        let engine = SyncEngine(
            identity: identity, contexts: contexts, config: config, events: continuation)
        let (port, task) = try await startSyncListener(
            localInfo: identity.peerInfo(),
            serverContext: contexts.server,
            port: config.tcpPort,
            syncFilesRoot: config.dataDir.appendingPathComponent(syncFilesDirName),
            delegate: engine)
        await engine.attachListener(port: port, task: task)
        return (engine, stream)
    }

    /// Fill in the listener details (used inside start)
    private func attachListener(port: UInt16, task: Task<Void, Never>) {
        boundPort = port
        listenerTask = task
    }

    /// Local device info
    public func localInfo() -> PeerInfo {
        identity.peerInfo()
    }

    /// Update the display name live (identity.json is the single source of
    /// truth for the nickname; settings.json does not hold it): persist first,
    /// update the in-memory state only after the write succeeds, and return the
    /// new device info for the discovery layer to replay
    ///
    /// Known small gap: the localInfo captured when the listener started does
    /// not follow the rename (an inbound handshake still answers with the old
    /// name, which is display-only), while the new name on the discovery
    /// channel corrects the peer's list right away.
    public func setDisplayName(_ name: String?) async throws -> PeerInfo {
        let dir = dataDir
        let reloaded = try await runBlocking { () -> Result<DeviceIdentity, any Error> in
            do {
                try DeviceIdentity.persistDisplayName(dir: dir, name: name)
                return .success(try DeviceIdentity.loadOrCreate(dir: dir))
            } catch {
                return .failure(error)
            }
        }.get()
        identity = reloaded
        return identity.peerInfo()
    }

    /// The port actually bound (the randomly assigned one when configured 0)
    public func port() -> UInt16 {
        boundPort
    }

    /// Graceful shutdown: stop listening and finish the event stream
    public func shutdown() {
        listenerTask?.cancel()
        events.finish()
    }

    // MARK: - Peer directory (driven by discovery; tests inject directly)

    /// A peer came online, or its info changed
    public func peerUp(_ peer: Peer) {
        peersByFingerprint[peer.info.fingerprint] = peer
        events.yield(.peerUp(peer))
    }

    /// A peer went offline
    public func peerDown(fingerprint: String) {
        guard peersByFingerprint.removeValue(forKey: fingerprint) != nil else { return }
        events.yield(.peerDown(fingerprint))
    }

    /// Snapshot of the peers currently online
    public func peers() -> [Peer] {
        Array(peersByFingerprint.values)
    }

    /// The current pairing list
    public func pairedList() -> [PairedPeer] {
        paired.list()
    }

    /// Snapshot of the inbound pairing requests waiting on a UI decision (the
    /// front end refetches this on mount as a fallback)
    public func pendingPairRequests() -> [PeerInfo] {
        pendingPairs.values.map(\.peer)
    }

    // MARK: - Pairing

    /// Start pairing with a given peer (blocks until the user on the peer
    /// decides, or the request times out)
    public func pair(fingerprint: String) async throws {
        guard let peer = peersByFingerprint[fingerprint] else {
            throw TransportError.peerUnreachable
        }
        let channel = try await dialPeer(
            addrs: peer.addrs, port: peer.port,
            clientContext: contexts.client, expectedFingerprint: fingerprint)
        let remote = try await pairTransaction(
            channel: channel, localInfo: identity.peerInfo(), expectedFingerprint: fingerprint)
        addPaired(remote)
    }

    /// Feed back the user's decision on an inbound pairing request (the answer
    /// to a pairRequested event)
    public func respondPair(fingerprint: String, accept: Bool) {
        guard var pending = pendingPairs[fingerprint],
            let continuation = pending.continuation
        else { return }
        pending.continuation = nil
        pending.timeout?.cancel()
        pending.timeout = nil
        pendingPairs[fingerprint] = pending
        continuation.resume(returning: accept)
    }

    /// Unpair: takes effect locally at once (that is the security boundary),
    /// then makes a best-effort attempt to notify the peer
    public func unpair(fingerprint: String) async {
        removePaired(fingerprint)
        guard let peer = peersByFingerprint[fingerprint] else { return }
        do {
            let channel = try await dialPeer(
                addrs: peer.addrs, port: peer.port,
                clientContext: contexts.client, expectedFingerprint: fingerprint)
            try await unpairTransaction(
                channel: channel, localInfo: identity.peerInfo(),
                expectedFingerprint: fingerprint)
        } catch {
            // Best effort: a failed notification is harmless, the peer's next
            // sync gets rejected anyway
        }
    }

    /// Record a pairing (idempotent) and emit the event
    private func addPaired(_ info: PeerInfo) {
        paired.insert(info)
        events.yield(.paired(info))
    }

    /// Remove a pairing and emit the event (a no-op when it does not exist)
    private func removePaired(_ fingerprint: String) {
        guard paired.remove(fingerprint) else { return }
        events.yield(.unpaired(fingerprint: fingerprint))
    }

    // MARK: - Sync

    /// Switch the sync direction policy; both directions take effect at once
    public func setSyncMode(_ mode: SyncMode) {
        sendEnabled = mode.sends
        recvEnabled = mode.receives
    }

    /// Switch the sync type toggles (send and receive change together)
    public func setSyncTypes(_ newTypes: SyncTypes) {
        types = newTypes
    }

    /// Update the cap on the total bytes of a file sync
    public func setMaxSyncFileBytes(_ bytes: UInt64) {
        maxSyncFileBytes = bytes
    }

    /// Cancel one echo registration (the shell layer calls this when its
    /// clipboard write fails, per the applyRemote contract)
    public func cancelEcho(hash: String) {
        if let idx = echo.firstIndex(where: { $0.hash == hash }) {
            echo.remove(at: idx)
        }
    }

    /// The outbound decision inside the pump (checks run before the broadcast
    /// path, the same shape as the Rust pump)
    private enum Outbound {
        /// Nothing goes out for this event
        case none
        /// Broadcast text
        case text(String)
        /// Broadcast an image (width / height / RGBA)
        case image(Int, Int, [UInt8])
        /// Broadcast files
        case files([String])
    }

    /// Entry point for local clipboard changes (the shell layer feeds it from
    /// the watcher task; tests inject directly): echo filter → LWW baseline
    /// update → history/UI event → broadcast by type
    public func clipboardChanged(_ event: ClipboardEvent) {
        // Echo: a remote write coming back around through the system clipboard.
        // Swallow it once and leave the LWW baseline alone
        if takeEcho(event.hash) {
            return
        }
        // max rather than a plain assignment: when the system clock steps back
        // (an NTP correction, or the user changing the time) a newer event can
        // carry a smaller timestamp, and assigning would drag the LWW baseline
        // backwards — older content on a peer could then overwrite what was
        // just copied here
        lastLocalCopyMs = max(lastLocalCopyMs, event.timestampMs)
        // Each of the three content kinds has its own sync pipeline (1.1); the
        // history pipeline consumes localCopied for all of them.
        // suppressBroadcast is the ignore-rule verdict: the baseline above
        // still advanced and localCopied still fires, only the outbound leg
        // is cut
        var outbound = Outbound.none
        if sendEnabled && !event.suppressBroadcast {
            switch event.content {
            case .text(let text) where types.text && text.utf8.count <= Config.maxSyncTextBytes:
                outbound = .text(text)
            case .image(let width, let height, let rgba) where types.images:
                outbound = .image(width, height, rgba)
            case .files(let paths) where types.files:
                outbound = .files(paths)
            default:
                break
            }
        }
        events.yield(
            .localCopied(
                content: event.content, hash: event.hash, timestampMs: event.timestampMs,
                suppressRecord: event.suppressRecord))
        switch outbound {
        case .none:
            break
        case .text(let text):
            broadcast(text: text, timestampMs: event.timestampMs)
        case .image(let width, let height, let rgba):
            Task { await self.broadcastImage(width: width, height: height, rgba: rgba, timestampMs: event.timestampMs) }
        case .files(let paths):
            Task { await self.broadcastFiles(paths: paths, timestampMs: event.timestampMs) }
        }
    }

    /// Broadcast one piece of local text to every peer that is paired and
    /// online (one concurrent dial per peer)
    private func broadcast(text: String, timestampMs: UInt64) {
        guard let (targets, offer) = broadcastSetup(contentType: ContentType.text, data: text, timestampMs: timestampMs)
        else { return }
        let localInfo = identity.peerInfo()
        let client = contexts.client
        for peer in targets {
            Task { [events] in
                do {
                    let channel = try await dialPeer(
                        addrs: peer.addrs, port: peer.port,
                        clientContext: client,
                        expectedFingerprint: peer.info.fingerprint)
                    try await syncTransaction(
                        channel: channel, localInfo: localInfo,
                        expectedFingerprint: peer.info.fingerprint, sync: offer)
                    events.yield(.syncSent(to: peer.info, error: nil))
                } catch {
                    events.yield(.syncSent(to: peer.info, error: describeSyncError(error)))
                }
            }
        }
    }

    /// Shared broadcast prelude: collect the paired-and-online targets and
    /// build the sync/offer frame (taking the next sequence number)
    private func broadcastSetup(
        contentType: String, data: String, timestampMs: UInt64
    ) -> ([Peer], ControlMessage)? {
        let targets = peersByFingerprint.values.filter { paired.contains($0.info.fingerprint) }
        guard !targets.isEmpty else { return nil }
        seq += 1
        let message = ControlMessage.clipboardSync(
            seq: seq, timestampMs: timestampMs, contentType: contentType, data: data)
        return (Array(targets), message)
    }

    /// Broadcast entry for an image copy: encode PNG (CPU-bound, on a blocking
    /// thread) → check the cap → blob broadcast
    private func broadcastImage(width: Int, height: Int, rgba: [UInt8], timestampMs: UInt64) async
    {
        let encoded = await runBlocking { encodePNG(width: width, height: height, rgba: rgba) }
        guard let png = encoded, UInt64(png.count) <= Config.maxSyncImageBytes else {
            // Encoding failed, or the result is over the cap: do not broadcast
            // (local history is unaffected)
            return
        }
        let meta = ImageOfferMeta(totalBytes: UInt64(png.count), width: width, height: height)
        guard let metaJSON = encodeOfferMeta(meta) else { return }
        broadcastBlob(
            kind: ContentType.image, metaJSON: metaJSON, metas: [],
            source: .memory(png), timestampMs: timestampMs)
    }

    /// Broadcast entry for a file copy: vet each file (regular file, total
    /// under the cap) → blob broadcast
    ///
    /// A batch holding a directory or a special file, or one over the cap, is
    /// not synced at all (local history still records it) — a partial sync
    /// would hand the peer a batch with pieces missing, so send nothing.
    private func broadcastFiles(paths: [String], timestampMs: UInt64) async {
        guard !paths.isEmpty, paths.count <= Config.maxSyncFileCount else { return }
        // Vetting runs on a blocking thread (one stat per file); symlinks are
        // resolved first, then checked for being regular files
        let checked = await runBlocking { () -> [FileMeta]? in
            var metas: [FileMeta] = []
            metas.reserveCapacity(paths.count)
            for path in paths {
                // Vetting follows symlinks (a link pointing at a regular file
                // is syncable), but the manifest keeps the original path's
                // name — what the user copied is the link itself
                let resolved = URL(fileURLWithPath: path).resolvingSymlinksInPath()
                guard
                    let values = try? resolved.resourceValues(forKeys: [
                        .isRegularFileKey, .fileSizeKey,
                    ]),
                    values.isRegularFile == true
                else { return nil }
                let name = URL(fileURLWithPath: path).lastPathComponent
                metas.append(FileMeta(name: name, bytes: UInt64(values.fileSize ?? 0)))
            }
            return metas
        }
        guard let metas = checked else { return }
        let total = metas.reduce(UInt64(0)) { $0 + $1.bytes }
        guard total > 0, total <= maxSyncFileBytes else { return }
        let offer = FilesOfferMeta(totalBytes: total, files: metas)
        guard let metaJSON = encodeOfferMeta(offer) else { return }
        broadcastBlob(
            kind: ContentType.files, metaJSON: metaJSON, metas: metas,
            source: .files(paths.map { URL(fileURLWithPath: $0) }), timestampMs: timestampMs)
    }

    /// Broadcast one blob (image or files) to every peer that is paired and
    /// online
    ///
    /// Each peer gets its own concurrent transaction, one peer's failure does
    /// not affect the others, and syncSent is emitted per peer (the same shape
    /// as the text broadcast; Data and arrays are copy-on-write, so peers share
    /// without cloning).
    private func broadcastBlob(
        kind: String, metaJSON: String, metas: [FileMeta], source: BlobSource,
        timestampMs: UInt64
    ) {
        guard let (targets, offer) = broadcastSetup(contentType: kind, data: metaJSON, timestampMs: timestampMs)
        else { return }
        let localInfo = identity.peerInfo()
        let client = contexts.client
        for peer in targets {
            Task { [events] in
                do {
                    let channel = try await dialPeer(
                        addrs: peer.addrs, port: peer.port,
                        clientContext: client,
                        expectedFingerprint: peer.info.fingerprint)
                    try await blobTransaction(
                        channel: channel, localInfo: localInfo,
                        expectedFingerprint: peer.info.fingerprint,
                        offer: offer, metas: metas, source: source)
                    events.yield(.syncSent(to: peer.info, error: nil))
                } catch {
                    events.yield(.syncSent(to: peer.info, error: describeSyncError(error)))
                }
            }
        }
    }

    /// Register the echo hash of one remote write (the oldest is evicted when
    /// the ring is full; a hash is registered only once — when several remotes
    /// sync the same text in turn, duplicate registrations leave orphans behind
    /// that swallow a genuine copy)
    private func pushEcho(_ hash: String) {
        pruneEcho()
        guard !echo.contains(where: { $0.hash == hash }) else { return }
        if echo.count >= Config.echoRecentCap {
            echo.removeFirst()
        }
        echo.append((hash, nowMs()))
    }

    /// Consume an echo on a hit (one shot)
    private func takeEcho(_ hash: String) -> Bool {
        pruneEcho()
        guard let idx = echo.firstIndex(where: { $0.hash == hash }) else { return false }
        echo.remove(at: idx)
        return true
    }

    /// Reclaim expired echo registrations (the orphans described on the
    /// [`echo`] field)
    ///
    /// The comparison saturates instead of subtracting outright: when the
    /// system clock steps back `now < atMs`, and unsigned subtraction
    /// underflows into a huge value that clears **every** registration — remote
    /// content would then be taken for a local copy and broadcast straight
    /// back, and the two ends would overwrite each other in a loop. Better to
    /// reclaim late than to reclaim wrongly.
    private func pruneEcho() {
        let now = nowMs()
        let ttlMs = UInt64(Config.echoTTL.components.seconds) * 1000
        echo.removeAll { now > $0.atMs && now - $0.atMs > ttlMs }
    }
}

/// Offer metadata → JSON string (nil on failure; callers treat that as "do not
/// broadcast")
private func encodeOfferMeta(_ meta: some Encodable) -> String? {
    (try? JSONEncoder().encode(meta)).flatMap { String(data: $0, encoding: .utf8) }
}

// MARK: - Accept-side decisions (ServerSessionDelegate)

extension SyncEngine: ServerSessionDelegate {
    /// Inbound pairing verdict: an existing pairing is accepted idempotently;
    /// otherwise raise a prompt to the UI and wait for the user.
    /// When the same peer sends concurrent duplicate requests the later one
    /// supersedes the earlier (which resumes as a rejection); cleanup goes by
    /// generation, so it only removes its own entry.
    public func decidePair(remote: PeerInfo) async -> Bool {
        if paired.contains(remote.fingerprint) {
            return true
        }
        pendingSeq += 1
        let generation = pendingSeq
        // Supersede: resume the earlier waiter as a rejection and stop its
        // timeout task
        var alreadyPrompted = false
        if var old = pendingPairs[remote.fingerprint] {
            alreadyPrompted = true
            old.timeout?.cancel()
            old.timeout = nil
            let oldCont = old.continuation
            old.continuation = nil
            pendingPairs[remote.fingerprint] = old
            oldCont?.resume(returning: false)
        }
        let accepted = await withCheckedContinuation {
            (continuation: CheckedContinuation<Bool, Never>) in
            // Decision timeout fallback: if nothing came back by then, resume
            // as a rejection (by generation, so it only touches its own)
            let timeout = Task { [weak self] in
                try? await Task.sleep(for: Config.pairDecisionTimeout)
                await self?.expirePendingPair(
                    fingerprint: remote.fingerprint, generation: generation)
            }
            pendingPairs[remote.fingerprint] = PendingPair(
                generation: generation, peer: remote, continuation: continuation,
                timeout: timeout)
            // Throttle: do not bother the user again while a prompt for this
            // peer is still unanswered — otherwise a peer that keeps
            // reconnecting and sending pairRequest can pin the user inside
            // modal prompts
            if !alreadyPrompted {
                events.yield(.pairRequested(remote))
            }
        }
        // Only clean up its own generation: once a concurrent request has
        // superseded it, the table holds the newcomer's record
        if pendingPairs[remote.fingerprint]?.generation == generation {
            pendingPairs[remote.fingerprint]?.timeout?.cancel()
            pendingPairs.removeValue(forKey: remote.fingerprint)
        }
        if accepted {
            addPaired(remote)
        }
        return accepted
    }

    /// Pairing decision timed out: resume as a rejection (a no-op when
    /// respondPair already handled it)
    private func expirePendingPair(fingerprint: String, generation: UInt64) {
        guard var pending = pendingPairs[fingerprint],
            pending.generation == generation,
            let continuation = pending.continuation
        else { return }
        pending.continuation = nil
        pending.timeout = nil
        pendingPairs[fingerprint] = pending
        continuation.resume(returning: false)
    }

    /// Inbound sync check chain: direction → pairing → type → size → LWW.
    /// Once it passes, **register the echo hash before emitting applyRemote**;
    /// an LWW skip returns nil, which replies Ack — the peer has no need to
    /// tell "applied" apart from "not applied because this side is newer".
    public func acceptSync(
        remote: PeerInfo, timestampMs: UInt64, contentKind: String, data: String
    ) async -> String? {
        guard recvEnabled else { return ReasonCode.disabled }
        guard paired.contains(remote.fingerprint) else { return ReasonCode.notPaired }
        // Images and files do not come through here (the session layer routes
        // them to the blob path by content_type); anything non-text reaching
        // this point is a new unknown kind and is always unsupported (an
        // evolution guardrail)
        guard contentKind == ContentType.text, types.text else {
            return ReasonCode.unsupportedType
        }
        guard data.utf8.count <= Config.maxSyncTextBytes else { return ReasonCode.tooLarge }
        // LWW: skip when the local copy is at least as recent (equal included,
        // one conservative overwrite fewer)
        guard timestampMs > lastLocalCopyMs else { return nil }
        let hash = hashText(data)
        pushEcho(hash)
        events.yield(
            .applyRemote(
                content: .text(data), from: remote, timestampMs: timestampMs, hash: hash))
        return nil
    }

    /// Blob offer verdict (three branches, **all decided before any stream is
    /// received**): direction → pairing → type toggles → size/count → LWW.
    /// The single-frame path can only reject after receiving everything;
    /// deciding at the offer stage stops a transfer that would be discarded.
    public func decideBlobOffer(
        remote: PeerInfo, timestampMs: UInt64, offer: BlobOffer
    ) async -> OfferDecision {
        guard recvEnabled else { return .reject(ReasonCode.disabled) }
        guard paired.contains(remote.fingerprint) else { return .reject(ReasonCode.notPaired) }
        switch offer {
        case .image(let meta):
            guard types.images else { return .reject(ReasonCode.unsupportedType) }
            guard meta.totalBytes > 0, meta.totalBytes <= Config.maxSyncImageBytes else {
                return .reject(ReasonCode.tooLarge)
            }
        case .files(let meta):
            guard types.files else { return .reject(ReasonCode.unsupportedType) }
            // The manifest comes from the peer, so the sum must guard against
            // overflow (an overflow is a malicious declaration: reject)
            var declared: UInt64 = 0
            for file in meta.files {
                let (sum, overflow) = declared.addingReportingOverflow(file.bytes)
                guard !overflow else { return .reject(ReasonCode.tooLarge) }
                declared = sum
            }
            // The manifest must sum to the declared total (the receiver splits
            // the stream by the manifest, so a mismatch means the stream
            // length is wrong and receiving it would be pointless)
            guard !meta.files.isEmpty, meta.files.count <= Config.maxSyncFileCount,
                meta.totalBytes > 0, meta.totalBytes <= maxSyncFileBytes,
                declared == meta.totalBytes
            else { return .reject(ReasonCode.tooLarge) }
        }
        // LWW up front: a stale verdict replies Ack (the same "the peer need
        // not tell them apart" semantics as text); the dialer sees Ack instead
        // of BlobAccept and knows there is nothing to transfer
        guard timestampMs > lastLocalCopyMs else { return .stale }
        return .accept
    }

    /// Land an image blob once its stream has been received: decode → echo
    /// registration → applyRemote
    ///
    /// A returned reason code makes the session layer reply SyncRejected.
    /// Decoding runs on a blocking thread (pure CPU work).
    public func finishBlobImage(
        remote: PeerInfo, timestampMs: UInt64, png: Data
    ) async -> String? {
        let decoded = await runBlocking { decodePNG(png) }
        guard let (width, height, rgba) = decoded else {
            return ReasonCode.unsupportedType
        }
        let content = ClipboardContent.image(width: width, height: height, rgba: rgba)
        let hash = content.hash()
        pushEcho(hash)
        events.yield(
            .applyRemote(content: content, from: remote, timestampMs: timestampMs, hash: hash))
        return nil
    }

    /// Land a file blob once its stream has been received: name the `.part`
    /// files → echo registration → applyRemote
    ///
    /// The echo hash is computed over the **local paths after landing** — those
    /// are exactly what the shell layer writes to the clipboard and what the
    /// watcher reads back around; the peer's paths mean nothing here.
    public func finishBlobFiles(
        remote: PeerInfo, timestampMs: UInt64, parts: [URL], metas: [FileMeta], batchDir: URL
    ) async -> String? {
        guard let finals = await finalizeParts(parts: parts, metas: metas, batchDir: batchDir)
        else {
            return ReasonCode.unsupportedType
        }
        let content = ClipboardContent.files(finals.map(\.path))
        let hash = content.hash()
        pushEcho(hash)
        events.yield(
            .applyRemote(content: content, from: remote, timestampMs: timestampMs, hash: hash))
        return nil
    }

    /// The peer notified us it unpaired
    public func unpaired(remote: PeerInfo) async {
        removePaired(remote.fingerprint)
    }
}

extension SyncEngine {
    /// Forward registry changes from the discovery layer into the engine
    /// (up → peerUp, down → peerDown)
    ///
    /// Returns the forwarding task; cancelling it detaches (the stream finishes
    /// on its own when the discovery service shuts down).
    public nonisolated func attachDiscovery(
        _ stream: AsyncStream<RegistryChange>
    ) -> Task<Void, Never> {
        Task { [weak self] in
            for await change in stream {
                guard let self else { return }
                switch change {
                case .up(let peer): await self.peerUp(peer)
                case .down(let fingerprint): await self.peerDown(fingerprint: fingerprint)
                }
            }
        }
    }
}

/// Readable description of a sync failure (reason codes pass through as they
/// are, everything else is normalized)
private func describeSyncError(_ error: Error) -> String {
    switch error {
    case TransportError.rejected(let code): code
    case TransportError.timeout(let step): "timeout(\(step))"
    case TransportError.peerUnreachable: "peer_unreachable"
    default: String(describing: error)
    }
}
