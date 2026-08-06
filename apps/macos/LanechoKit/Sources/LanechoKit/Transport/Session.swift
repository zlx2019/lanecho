// Session transaction layer (mirroring the Rust sync/net.rs)
//
// Connections are dial-transact-leave: every transaction opens a fresh TLS
// connection, passes the Hello gate (version plus "declared fingerprint ==
// certificate fingerprint"), runs the transaction, and finishes with Bye plus a
// drain to EOF. Engine decisions (the pairing verdict, the sync check chain)
// are injected through ServerSessionDelegate; this module only orchestrates IO
// — the same split as net.rs / Inner on the Rust side.

import Foundation
import NIOCore
import NIOPosix
import NIOSSL

/// Transport-layer errors (engine-layer errors are aggregated in SyncEngine)
public enum TransportError: Error {
    /// A step timed out (the parameter names which one)
    case timeout(String)
    /// Peer unreachable (every candidate address failed to connect)
    case peerUnreachable
    /// The fingerprint the peer declared disagrees with its TLS certificate or
    /// with the expected one (likely an impostor)
    case fingerprintMismatch
    /// The user on the peer rejected the pairing request
    case pairRejected
    /// The peer rejected the sync (the parameter is a structured reason code)
    case rejected(String)
    /// The peer disconnected early
    case disconnected
    /// Local IO failure (reading or writing the blob stream to disk)
    case io(String)
}

/// Frame channel: a bidirectional message stream whose pipeline already has TLS
/// and the frame codec installed (raw-stream chunks included since 1.1)
public typealias FrameChannel = NIOAsyncChannel<WireMessage, WireMessage>

/// Run with a deadline (on timeout, throw TransportError.timeout and cancel the
/// operation)
public func withDeadline<T: Sendable>(
    _ duration: Duration, step: String,
    _ op: @escaping @Sendable () async throws -> T
) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await op() }
        group.addTask {
            try await Task.sleep(for: duration)
            throw TransportError.timeout(step)
        }
        guard let result = try await group.next() else {
            throw TransportError.timeout(step)
        }
        group.cancelAll()
        return result
    }
}

/// Inbound frame reader: boxes the non-Sendable iterator so deadline-bounded
/// reads can use it across tasks
///
/// Access is strictly serialized (one read at a time; the timeout task never
/// touches the iterator), which makes the box safe.
public final class FrameReader: @unchecked Sendable {
    private var iterator: NIOAsyncChannelInboundStream<WireMessage>.AsyncIterator

    /// Build from an inbound stream (one per connection)
    public init(_ stream: NIOAsyncChannelInboundStream<WireMessage>) {
        self.iterator = stream.makeAsyncIterator()
    }

    /// Read the next pipeline message (nil at EOF)
    public func next() async throws -> WireMessage? {
        try await iterator.next()
    }

    /// Read the next frame with a deadline; EOF means the peer disconnected and
    /// a raw-stream chunk is a protocol violation (the decoder only emits
    /// `.raw` once this end has deliberately entered blob receive mode)
    public func nextFrame(within duration: Duration, step: String) async throws -> ControlMessage
    {
        try await withDeadline(duration, step: step) { [self] in
            guard let message = try await next() else {
                throw TransportError.disconnected
            }
            guard case .frame(let frame) = message else {
                throw ProtocolError.unexpectedMessage(expected: step, got: "raw")
            }
            return frame
        }
    }

    /// Read the next raw-stream chunk with a deadline (blob receive stage only)
    public func nextRaw(within duration: Duration, step: String) async throws -> ByteBuffer {
        try await withDeadline(duration, step: step) { [self] in
            guard let message = try await next() else {
                throw TransportError.disconnected
            }
            guard case .raw(let chunk) = message else {
                throw ProtocolError.unexpectedMessage(expected: "raw", got: "frame")
            }
            return chunk
        }
    }

    /// Drain to EOF (the receive half of a graceful close; bounded in case the
    /// peer does not cooperate)
    public func drainToEOF() async {
        try? await withDeadline(.seconds(3), step: "drain") { [self] in
            while try await next() != nil {}
        }
    }
}

// MARK: - Dial side

/// Dial the candidate addresses in order and install the TLS + frame pipeline
///
/// A non-nil `expectedFingerprint` is pinned in the TLS verification callback;
/// the liveness probe passes nil, because its liveness decision rests on an
/// explicit comparison made after the handshake.
public func dialPeer(
    addrs: [String], port: UInt16,
    clientContext: NIOSSLContext,
    expectedFingerprint: String?,
    onServerFingerprint: (@Sendable (String) -> Void)? = nil,
    group: any EventLoopGroup = MultiThreadedEventLoopGroup.singleton
) async throws -> FrameChannel {
    for addr in addrs {
        do {
            return try await withDeadline(Config.connectTimeout, step: "connect") {
                try await ClientBootstrap(group: group)
                    .connectTimeout(.seconds(3))
                    .connect(host: addr, port: Int(port)) { channel in
                        channel.eventLoop.makeCompletedFuture {
                            let ssl = try NIOSSLClientHandler(
                                context: clientContext,
                                serverHostname: nil,
                                customVerificationCallback: pinnedCertificate(
                                    expected: expectedFingerprint,
                                    onFingerprint: onServerFingerprint))
                            let (decoder, encoder) = makeFrameHandlers()
                            try channel.pipeline.syncOperations.addHandlers([
                                ssl, decoder, encoder,
                            ])
                            return try FrameChannel(wrappingChannelSynchronously: channel)
                        }
                    }
            }
        } catch {
            // This address failed; try the next candidate
            continue
        }
    }
    throw TransportError.peerUnreachable
}

/// Outbound handshake: Hello → HelloAck, checking the version and "declared
/// fingerprint == pinned fingerprint"
private func handshakeOut(
    reader: FrameReader,
    outbound: NIOAsyncChannelOutboundWriter<WireMessage>,
    localInfo: PeerInfo,
    expectedFingerprint: String
) async throws -> PeerInfo {
    try await outbound.write(.frame(.hello(version: Config.protocolVersion, info: localInfo)))
    let reply = try await reader.nextFrame(within: Config.replyTimeout, step: "hello_ack")
    guard case .helloAck(let version, let info) = reply else {
        throw ProtocolError.unexpectedMessage(expected: "hello_ack", got: reply.kind)
    }
    guard isVersionCompatible(version) else {
        throw ProtocolError.versionMismatch(peer: version)
    }
    // TLS already guarantees the certificate matches the pinned fingerprint;
    // this check ties the declaration to the certificate (anti-impersonation)
    guard info.fingerprint == expectedFingerprint else {
        throw TransportError.fingerprintMismatch
    }
    return info
}

/// Wrap up: Bye plus a drain to EOF (closing outright sends an RST that wipes
/// out frames still in flight)
private func gracefulClose(
    reader: FrameReader, outbound: NIOAsyncChannelOutboundWriter<WireMessage>
) async {
    try? await outbound.write(.frame(.bye))
    outbound.finish()
    await reader.drainToEOF()
}

/// Sync transaction (dial side): deliver the pre-encoded sync content and wait
/// for the peer's verdict
///
/// `sync` is the message itself (a broadcast sends the same content to every
/// target, so the engine builds it once and shares it).
public func syncTransaction(
    channel: FrameChannel, localInfo: PeerInfo, expectedFingerprint: String,
    sync: ControlMessage
) async throws {
    try await channel.executeThenClose { inbound, outbound in
        let reader = FrameReader(inbound)
        _ = try await handshakeOut(
            reader: reader, outbound: outbound,
            localInfo: localInfo, expectedFingerprint: expectedFingerprint)
        try await outbound.write(.frame(sync))
        let reply = try await reader.nextFrame(within: Config.replyTimeout, step: "sync_reply")
        var outcome: Result<Void, TransportError>
        switch reply {
        case .syncAck:
            outcome = .success(())
        case .syncRejected(let reasonCode):
            outcome = .failure(.rejected(reasonCode))
        default:
            throw ProtocolError.unexpectedMessage(expected: "sync_ack", got: reply.kind)
        }
        await gracefulClose(reader: reader, outbound: outbound)
        try outcome.get()
    }
}

/// Data source for a blob broadcast (shared across the per-peer transactions;
/// Data and arrays are copy-on-write, so nothing is cloned per peer)
public enum BlobSource: Sendable {
    /// Bytes in memory (an image PNG)
    case memory(Data)
    /// Files on disk (each peer streams its own read; with the usual one or two
    /// peers a shared read is not worth it)
    case files([URL])
}

/// Blob sync transaction (dial side): offer → one of three branches → raw
/// stream → footer → final verdict
///
/// Against a 1.0 peer the offer is still a valid frame: it sees an unknown
/// content_type and replies `sync_rejected(unsupported_type)`, which this
/// function returns as an ordinary rejection — compatibility rides on the
/// protocol's own rejection path, with no version sniffing.
public func blobTransaction(
    channel: FrameChannel, localInfo: PeerInfo, expectedFingerprint: String,
    offer: ControlMessage, metas: [FileMeta], source: BlobSource
) async throws {
    try await channel.executeThenClose { inbound, outbound in
        let reader = FrameReader(inbound)
        _ = try await handshakeOut(
            reader: reader, outbound: outbound,
            localInfo: localInfo, expectedFingerprint: expectedFingerprint)
        try await outbound.write(.frame(offer))
        let reply = try await reader.nextFrame(
            within: Config.replyTimeout, step: "blob_offer_reply")
        let outcome: Result<Void, any Error>
        switch reply {
        // LWW found it stale on the peer: it does not want the content, so
        // nothing is transferred (keeping the "Ack means success" semantics)
        case .syncAck:
            outcome = .success(())
        case .syncRejected(let reasonCode):
            outcome = .failure(TransportError.rejected(reasonCode))
        case .blobAccept:
            do {
                try await sendBlobBody(
                    reader: reader, outbound: outbound, metas: metas, source: source)
                outcome = .success(())
            } catch {
                outcome = .failure(error)
            }
        default:
            throw ProtocolError.unexpectedMessage(expected: "blob_accept", got: reply.kind)
        }
        await gracefulClose(reader: reader, outbound: outbound)
        try outcome.get()
    }
}

/// Blob raw stream + footer + final reply (the dial side's transfer stage)
private func sendBlobBody(
    reader: FrameReader,
    outbound: NIOAsyncChannelOutboundWriter<WireMessage>,
    metas: [FileMeta],
    source: BlobSource
) async throws {
    let hash: String
    switch source {
    case .memory(let bytes):
        hash = try await sendBlobBytes(outbound: outbound, bytes: bytes)
    case .files(let paths):
        hash = try await sendBlobFiles(outbound: outbound, paths: paths, metas: metas)
    }
    try await outbound.write(.frame(.blobFooter(hash: hash)))
    let reply = try await reader.nextFrame(within: Config.replyTimeout, step: "blob_final_reply")
    switch reply {
    case .syncAck:
        return
    case .syncRejected(let reasonCode):
        throw TransportError.rejected(reasonCode)
    default:
        throw ProtocolError.unexpectedMessage(expected: "sync_ack", got: reply.kind)
    }
}

/// Stream a block of in-memory bytes (an image PNG); returns the BLAKE3 hex of
/// the whole stream
private func sendBlobBytes(
    outbound: NIOAsyncChannelOutboundWriter<WireMessage>, bytes: Data
) async throws -> String {
    let hasher = Blake3.Hasher()
    var offset = bytes.startIndex
    while offset < bytes.endIndex {
        let end =
            bytes.index(offset, offsetBy: Config.syncStreamBuf, limitedBy: bytes.endIndex)
            ?? bytes.endIndex
        let chunk = bytes[offset..<end]
        hasher.update(chunk)
        try await outbound.write(.raw(ByteBuffer(bytes: chunk)))
        offset = end
    }
    return hasher.finalizeHex()
}

/// Boxes the non-Sendable FileHandle for use across runBlocking (access is
/// strictly serialized)
private final class HandleBox: @unchecked Sendable {
    let handle: FileHandle
    init(_ handle: FileHandle) { self.handle = handle }
}

/// Stream a set of files (concatenated in meta order); returns the BLAKE3 hex
/// of the whole stream
///
/// Both ways a file can change underneath an in-flight send must abort rather
/// than muddle through:
/// - It shrank (EOF before the declared byte count): sending on would leave the
///   total stream length wrong and the peer's read would hang until it times
///   out — throw and drop the connection instead, so the peer cleans the batch
///   up as unfinished;
/// - It grew: send only the declared byte count (the extra bytes are not part
///   of this snapshot).
private func sendBlobFiles(
    outbound: NIOAsyncChannelOutboundWriter<WireMessage>, paths: [URL], metas: [FileMeta]
) async throws -> String {
    let hasher = Blake3.Hasher()
    for (path, meta) in zip(paths, metas) {
        let opened = await runBlocking { () -> HandleBox? in
            (try? FileHandle(forReadingFrom: path)).map(HandleBox.init)
        }
        guard let box = opened else {
            throw TransportError.io("Unable to open file: \(path.path)")
        }
        // close is a single fast syscall; not worth hopping threads for
        defer { try? box.handle.close() }
        var remaining = meta.bytes
        while remaining > 0 {
            let want = Int(min(remaining, UInt64(Config.syncStreamBuf)))
            let data = await runBlocking { () -> Data? in
                try? box.handle.read(upToCount: want)
            }
            guard let data, !data.isEmpty else {
                throw TransportError.io("File shrank while being sent: \(path.path)")
            }
            hasher.update(data)
            try await outbound.write(.raw(ByteBuffer(bytes: data)))
            remaining -= UInt64(data.count)
        }
    }
    return hasher.finalizeHex()
}

/// Receive a raw byte stream into memory (images; the caller already checked
/// total against the cap during the offer) and return the bytes plus the BLAKE3
/// hex of the whole stream. Each read carries the same timeout as the
/// inter-frame gap, so a stream cut short does not hang
func recvBlobBytes(reader: FrameReader, total: UInt64) async throws -> (Data, String) {
    let hasher = Blake3.Hasher()
    var out = Data(capacity: Int(total))
    var remaining = Int(total)
    while remaining > 0 {
        var chunk = try await reader.nextRaw(within: Config.idleTimeout, step: "blob_stream")
        let bytes = chunk.readBytes(length: chunk.readableBytes) ?? []
        hasher.update(bytes)
        out.append(contentsOf: bytes)
        remaining -= bytes.count
    }
    return (out, hasher.finalizeHex())
}

/// Receive a raw byte stream, split it by the manifest and write it into
/// `.part` temporary files as it arrives
///
/// Returns (the temporary file paths, the BLAKE3 hex of the whole stream).
/// Nothing gets its final name before the checksum passes — the caller cleans
/// up: [`finalizeParts`] on success, delete the whole batch directory on
/// failure. Raw-stream chunks have nothing to do with file boundaries (the
/// decoder emits them as they arrive), so this function does the splitting.
func recvBlobFiles(
    reader: FrameReader, metas: [FileMeta], batchDir: URL
) async throws -> ([URL], String) {
    let prepared = await runBlocking {
        (try? FileManager.default.createDirectory(
            at: batchDir, withIntermediateDirectories: true)) != nil
    }
    guard prepared else {
        throw TransportError.io("Unable to create batch directory: \(batchDir.path)")
    }
    let hasher = Blake3.Hasher()
    var parts: [URL] = []
    parts.reserveCapacity(metas.count)
    // Tail of the previous chunk left unconsumed (a chunk can straddle a file
    // boundary)
    var pending = ByteBuffer()
    for (index, meta) in metas.enumerated() {
        let part = batchDir.appendingPathComponent("recv-\(index).part")
        let opened = await runBlocking { () -> HandleBox? in
            guard FileManager.default.createFile(atPath: part.path, contents: nil) else {
                return nil
            }
            return (try? FileHandle(forWritingTo: part)).map(HandleBox.init)
        }
        guard let box = opened else {
            throw TransportError.io("Unable to create temporary file: \(part.path)")
        }
        var remaining = meta.bytes
        var writeFailed = false
        while remaining > 0 {
            if pending.readableBytes == 0 {
                pending = try await reader.nextRaw(
                    within: Config.idleTimeout, step: "blob_stream")
            }
            let take = Int(min(remaining, UInt64(pending.readableBytes)))
            let data = Data(pending.readBytes(length: take) ?? [])
            hasher.update(data)
            let written = await runBlocking {
                (try? box.handle.write(contentsOf: data)) != nil
            }
            guard written else {
                writeFailed = true
                break
            }
            remaining -= UInt64(take)
        }
        await runBlocking { try? box.handle.close() }
        if writeFailed {
            throw TransportError.io("Failed to write temporary file: \(part.path)")
        }
        parts.append(part)
    }
    return (parts, hasher.finalizeHex())
}

/// Pairing transaction (dial side): send the request and wait for the user on
/// the peer to answer the prompt (human in the loop, hence the long timeout)
public func pairTransaction(
    channel: FrameChannel, localInfo: PeerInfo, expectedFingerprint: String
) async throws -> PeerInfo {
    try await channel.executeThenClose { inbound, outbound in
        let reader = FrameReader(inbound)
        let remote = try await handshakeOut(
            reader: reader, outbound: outbound,
            localInfo: localInfo, expectedFingerprint: expectedFingerprint)
        try await outbound.write(.frame(.pairRequest))
        let reply = try await reader.nextFrame(
            within: Config.pairDecisionTimeout, step: "pair_response")
        guard case .pairResponse(let accepted) = reply else {
            throw ProtocolError.unexpectedMessage(expected: "pair_response", got: reply.kind)
        }
        await gracefulClose(reader: reader, outbound: outbound)
        guard accepted else { throw TransportError.pairRejected }
        return remote
    }
}

/// Unpair notification (dial side, best effort): a failure is harmless — the
/// security boundary sits on the receiving side
public func unpairTransaction(
    channel: FrameChannel, localInfo: PeerInfo, expectedFingerprint: String
) async throws {
    try await channel.executeThenClose { inbound, outbound in
        let reader = FrameReader(inbound)
        _ = try await handshakeOut(
            reader: reader, outbound: outbound,
            localInfo: localInfo, expectedFingerprint: expectedFingerprint)
        try await outbound.write(.frame(.unpair))
        await gracefulClose(reader: reader, outbound: outbound)
    }
}

// MARK: - Accept side

/// A parsed blob offer (serveBlob decodes it out of clipboard_sync.data)
public enum BlobOffer: Sendable {
    /// An image (a PNG stream)
    case image(ImageOfferMeta)
    /// A file batch
    case files(FilesOfferMeta)
}

/// Verdict on a blob offer (three branches)
public enum OfferDecision: Sendable {
    /// Wanted: reply BlobAccept and ask the peer for the stream
    case accept
    /// LWW found it stale: reply SyncAck and the peer goes straight to Bye
    case stale
    /// Rejected: reply SyncRejected(code)
    case reject(String)
}

/// Injection point for engine decisions on the accept side (the counterpart of
/// Inner on the Rust side)
public protocol ServerSessionDelegate: Sendable {
    /// Inbound pairing verdict: an existing pairing is accepted idempotently;
    /// otherwise raise it to the UI and wait for the user (can be a long wait)
    func decidePair(remote: PeerInfo) async -> Bool
    /// Inbound sync check chain (kill switch → pairing → type → size → LWW);
    /// returns nil when it passes and a reason code when it rejects (a stale
    /// LWW verdict also returns nil, which replies Ack)
    func acceptSync(remote: PeerInfo, timestampMs: UInt64, contentKind: String, data: String)
        async -> String?
    /// Blob offer verdict (**decided in full before any stream is received**)
    func decideBlobOffer(remote: PeerInfo, timestampMs: UInt64, offer: BlobOffer) async
        -> OfferDecision
    /// Land an image blob once the received stream checks out (decode → echo
    /// registration → applyRemote); nil replies Ack, a reason code replies
    /// SyncRejected
    func finishBlobImage(remote: PeerInfo, timestampMs: UInt64, png: Data) async -> String?
    /// Land a file blob once the received stream checks out (final naming →
    /// echo registration → applyRemote)
    func finishBlobFiles(
        remote: PeerInfo, timestampMs: UInt64, parts: [URL], metas: [FileMeta], batchDir: URL
    ) async -> String?
    /// The peer notified us it unpaired
    func unpaired(remote: PeerInfo) async
}

/// Serve one connection (accept side): pass the Hello gate (deadline-bounded),
/// then run the transaction loop
///
/// `fingerprintBox` is filled by the TLS verification callback; **read it only
/// after the first frame (Hello) has arrived** — the TLS handshake is lazy (it
/// happens on the first I/O), so when the serve task starts the callback has
/// usually not run yet, whereas a decrypted frame proves the handshake and its
/// callback both completed. The inter-frame gap is bounded at 60s, so a
/// half-open connection cannot sit on a quota slot forever (waiting on the user
/// for pairing happens inside decidePair and is not subject to this timeout).
func serveConnection(
    channel: FrameChannel,
    localInfo: PeerInfo,
    fingerprintBox: FingerprintBox,
    syncFilesRoot: URL,
    delegate: any ServerSessionDelegate
) async {
    try? await channel.executeThenClose { inbound, outbound in
        let reader = FrameReader(inbound)
        // Hello gate: the whole unauthenticated phase is deadline-bounded
        let first = try await reader.nextFrame(within: Config.handshakeTimeout, step: "hello")
        guard case .hello(let version, let remote) = first else {
            return
        }
        guard isVersionCompatible(version) else { return }
        // The declared fingerprint must match the TLS certificate
        // (anti-impersonation); a decrypted frame implies the callback has
        // already filled the box
        guard let clientFingerprint = fingerprintBox.get(),
            remote.fingerprint == clientFingerprint
        else { return }
        try await outbound.write(
            .frame(.helloAck(version: Config.protocolVersion, info: localInfo)))

        // Transaction loop: a peer usually runs one transaction then says Bye;
        // a read failure or timeout goes straight to the wrap-up
        loop: while true {
            let message: ControlMessage
            do {
                message = try await reader.nextFrame(within: Config.idleTimeout, step: "idle")
            } catch {
                break
            }
            switch message {
            case .pairRequest:
                let accepted = await delegate.decidePair(remote: remote)
                try await outbound.write(.frame(.pairResponse(accepted: accepted)))
            case .clipboardSync(_, let timestampMs, let contentType, let data):
                // Images and files come as blob offers (1.1): three-branch
                // verdict plus a stream receive; text and unknown types stay
                // on the single-frame check chain
                if contentType == ContentType.image || contentType == ContentType.files {
                    do {
                        try await serveBlob(
                            reader: reader, outbound: outbound, remote: remote,
                            timestampMs: timestampMs, kind: contentType, data: data,
                            syncFilesRoot: syncFilesRoot, delegate: delegate)
                    } catch {
                        // IO failure or protocol violation: the connection
                        // cannot continue
                        break loop
                    }
                    continue
                }
                let rejection = await delegate.acceptSync(
                    remote: remote, timestampMs: timestampMs,
                    contentKind: contentType, data: data)
                switch rejection {
                case .none:
                    try await outbound.write(.frame(.syncAck))
                case .some(let reasonCode):
                    try await outbound.write(.frame(.syncRejected(reasonCode: reasonCode)))
                }
            case .unpair:
                await delegate.unpaired(remote: remote)
            case .bye:
                break loop
            default:
                // Unexpected message; drop the connection
                break loop
            }
        }
        outbound.finish()
        await reader.drainToEOF()
    }
}

/// Orchestrate accepting a blob offer (accept side): verdict → receive →
/// checksum → land → final reply
///
/// A throw means the connection cannot continue (IO failure or protocol
/// violation) and the caller leaves the transaction loop. Ordinary rejections
/// (type toggle off, over the cap, checksum mismatch) reply SyncRejected and
/// return normally.
private func serveBlob(
    reader: FrameReader,
    outbound: NIOAsyncChannelOutboundWriter<WireMessage>,
    remote: PeerInfo,
    timestampMs: UInt64,
    kind: String,
    data: String,
    syncFilesRoot: URL,
    delegate: any ServerSessionDelegate
) async throws {
    // Metadata that fails to parse is a protocol violation (a 1.1 dialer never
    // sends bad meta): drop the connection
    let offer: BlobOffer
    if kind == ContentType.image {
        guard let meta = try? JSONDecoder().decode(ImageOfferMeta.self, from: Data(data.utf8))
        else {
            throw ProtocolError.unexpectedMessage(expected: "image_offer_meta", got: "bad_json")
        }
        offer = .image(meta)
    } else {
        guard let meta = try? JSONDecoder().decode(FilesOfferMeta.self, from: Data(data.utf8))
        else {
            throw ProtocolError.unexpectedMessage(expected: "files_offer_meta", got: "bad_json")
        }
        offer = .files(meta)
    }
    switch await delegate.decideBlobOffer(remote: remote, timestampMs: timestampMs, offer: offer)
    {
    case .reject(let code):
        try await outbound.write(.frame(.syncRejected(reasonCode: code)))
        return
    case .stale:
        try await outbound.write(.frame(.syncAck))
        return
    case .accept:
        break
    }

    // Receive the stream (the decoder switches into raw-stream mode and counts
    // out exactly the total the offer declared)
    switch offer {
    case .image(let meta):
        try await outbound.write(.blobAcceptExpectingRaw(bytes: Int(meta.totalBytes)))
        let (png, actualHash) = try await recvBlobBytes(reader: reader, total: meta.totalBytes)
        let footer = try await expectFooter(reader)
        let reply: ControlMessage
        if footer != actualHash {
            reply = .syncRejected(reasonCode: ReasonCode.checksumMismatch)
        } else if let code = await delegate.finishBlobImage(
            remote: remote, timestampMs: timestampMs, png: png)
        {
            reply = .syncRejected(reasonCode: code)
        } else {
            reply = .syncAck
        }
        try await outbound.write(.frame(reply))
    case .files(let meta):
        try await outbound.write(.blobAcceptExpectingRaw(bytes: Int(meta.totalBytes)))
        // Batch directory: a failed checksum or a mid-stream disconnect
        // deletes the whole directory, leaving no half-written files
        let batchName = String(
            UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased().prefix(8))
        let batchDir = syncFilesRoot.appendingPathComponent(batchName)
        let received: ([URL], String)
        let footer: String
        do {
            received = try await recvBlobFiles(
                reader: reader, metas: meta.files, batchDir: batchDir)
            footer = try await expectFooter(reader)
        } catch {
            await removeBatchDir(batchDir)
            throw error
        }
        let (parts, actualHash) = received
        let reply: ControlMessage
        if footer != actualHash {
            await removeBatchDir(batchDir)
            reply = .syncRejected(reasonCode: ReasonCode.checksumMismatch)
        } else if let code = await delegate.finishBlobFiles(
            remote: remote, timestampMs: timestampMs, parts: parts,
            metas: meta.files, batchDir: batchDir)
        {
            await removeBatchDir(batchDir)
            reply = .syncRejected(reasonCode: code)
        } else {
            reply = .syncAck
        }
        try await outbound.write(.frame(reply))
    }
}

/// Read and check the BlobFooter frame, returning the hash it carries
private func expectFooter(_ reader: FrameReader) async throws -> String {
    let frame = try await reader.nextFrame(within: Config.idleTimeout, step: "blob_footer")
    guard case .blobFooter(let hash) = frame else {
        throw ProtocolError.unexpectedMessage(expected: "blob_footer", got: frame.kind)
    }
    return hash
}

/// Delete a receive batch directory (cleanup on the failure paths, best effort)
private func removeBatchDir(_ dir: URL) async {
    await runBlocking { try? FileManager.default.removeItem(at: dir) }
}

/// Inbound connection quota (mirroring the Rust side's
/// `Semaphore::new(MAX_CONCURRENT_CONNECTIONS)`)
///
/// The accept side takes **any** client certificate (the pairing model is
/// decided at the application layer), so completing a TLS handshake needs no
/// pairing. Without a quota, any host on the segment can connect and then never
/// send Hello, and each such connection holds a slot for the full 30s handshake
/// timeout — a slightly higher connection rate piles up without bound and
/// exhausts the fd table.
final class ConnectionQuota: @unchecked Sendable {
    private let lock = NSLock()
    private var inUse = 0
    private let limit: Int

    /// Build with a limit
    init(limit: Int) {
        self.limit = limit
    }

    /// Try to take a slot (false when full; the caller must close the
    /// connection outright)
    func acquire() -> Bool {
        lock.withLock {
            guard inUse < limit else { return false }
            inUse += 1
            return true
        }
    }

    /// Give a slot back
    func release() {
        lock.withLock { inUse = max(0, inUse - 1) }
    }
}

/// Start the accept listener; returns (bound port, listener task handle)
///
/// Each inbound connection's client fingerprint is captured in the TLS
/// verification callback (one box per connection) and handed to serveConnection
/// after authentication. [`ConnectionQuota`] caps the number of concurrent
/// connections and anything over the cap is closed immediately (slow-loris
/// defence).
/// - `maxConnections`: cap on concurrent connections, only lowered by tests;
///   production uses the Config default
public func startSyncListener(
    localInfo: PeerInfo,
    serverContext: NIOSSLContext,
    port: UInt16,
    syncFilesRoot: URL,
    delegate: any ServerSessionDelegate,
    maxConnections: Int = Config.maxConcurrentConnections,
    group: any EventLoopGroup = MultiThreadedEventLoopGroup.singleton
) async throws -> (port: UInt16, task: Task<Void, Never>) {
    // Per-connection fingerprint box: the verification callback runs before
    // serveConnection, so it is bound while the pipeline is assembled
    let server = try await ServerBootstrap(group: group)
        .serverChannelOption(.socketOption(.so_reuseaddr), value: 1)
        .bind(host: "0.0.0.0", port: Int(port)) { channel in
            channel.eventLoop.makeCompletedFuture {
                let fingerprintBox = FingerprintBox()
                let ssl = NIOSSLServerHandler(
                    context: serverContext,
                    customVerificationCallback: acceptAnyCertificate { fp in
                        fingerprintBox.set(fp)
                    })
                let (decoder, encoder) = makeFrameHandlers()
                try channel.pipeline.syncOperations.addHandlers([
                    ssl, decoder, encoder,
                ])
                let wrapped = try FrameChannel(wrappingChannelSynchronously: channel)
                return AcceptedConnection(channel: wrapped, fingerprint: fingerprintBox)
            }
        }
    let boundPort = UInt16(server.channel.localAddress?.port ?? 0)
    let quota = ConnectionQuota(limit: maxConnections)
    let task = Task { () -> Void in
        try? await server.executeThenClose { inbound in
            try await withThrowingDiscardingTaskGroup { connections in
                for try await accepted in inbound {
                    // Quota full: close at once, so unauthenticated
                    // connections never queue up holding resources
                    guard quota.acquire() else {
                        try? await accepted.channel.executeThenClose { _, outbound in
                            outbound.finish()
                        }
                        continue
                    }
                    connections.addTask {
                        defer { quota.release() }
                        await serveConnection(
                            channel: accepted.channel,
                            localInfo: localInfo,
                            fingerprintBox: accepted.fingerprint,
                            syncFilesRoot: syncFilesRoot,
                            delegate: delegate)
                    }
                }
            }
        }
    }
    return (boundPort, task)
}

/// Per-connection fingerprint box (carries the value from the TLS verification
/// callback to serveConnection)
final class FingerprintBox: @unchecked Sendable {
    private let lock = NSLock()
    private var fingerprint: String?

    /// Written by the verification callback
    func set(_ fp: String) {
        lock.lock()
        defer { lock.unlock() }
        fingerprint = fp
    }

    /// Read by the accept side
    func get() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return fingerprint
    }
}

/// An accepted connection (the frame channel plus its TLS client fingerprint
/// box)
struct AcceptedConnection: Sendable {
    /// The frame channel
    let channel: FrameChannel
    /// Client fingerprint (filled in by the verification callback)
    let fingerprint: FingerprintBox
}
