// Control channel messages.
//
// The JSON shape matches the Rust
// `#[serde(tag = "type", rename_all = "snake_case")]` exactly:
// `{"type":"<variant name>", ...fields flattened}`. An unknown type fails to
// decode, matching serde's behaviour for an unknown variant. Field keys are
// snake_case.

import Foundation

/// Sync content type, deliberately left open as a string
public enum ContentType {
    /// Plain text (the only sync type v1 supports)
    public static let text = "text"
    /// Image (since 1.1); the offer's data is [`ImageOfferMeta`] JSON
    public static let image = "image"
    /// Files (since 1.1); the offer's data is [`FilesOfferMeta`] JSON
    public static let files = "files"
}

/// Structured rejection reason codes (an open string; unknown codes render as
/// a generic failure)
public enum ReasonCode {
    /// The source is not paired
    public static let notPaired = "not_paired"
    /// The payload exceeds the receiver's limit
    public static let tooLarge = "too_large"
    /// The receiver has paused syncing
    public static let disabled = "disabled"
    /// Unsupported content type: a 1.0 peer received a blob offer, or a type
    /// toggle is off
    public static let unsupportedType = "unsupported_type"
    /// Blob stream integrity check failed (since 1.1; only on blob
    /// transactions)
    public static let checksumMismatch = "checksum_mismatch"
}

/// Image offer metadata: a JSON string carried in clipboard_sync.data (1.1)
///
/// To a 1.0 peer the offer is a **perfectly valid frame**: it sees an unknown
/// content_type, replies `sync_rejected(unsupported_type)`, and the dialer
/// skips that peer. Compatibility does not rest on version sniffing but on the
/// protocol's own rejection path.
public struct ImageOfferMeta: Codable, Sendable, Equatable {
    /// Total length of the raw byte stream that follows (PNG-encoded byte
    /// count)
    public var totalBytes: UInt64
    /// Pixel width, for pre-checks and notification text; what lands is
    /// whatever the decode produces
    public var width: Int
    /// Pixel height
    public var height: Int

    /// Wire key names (matching the Rust serde snake_case)
    enum CodingKeys: String, CodingKey {
        case totalBytes = "total_bytes"
        case width
        case height
    }

    /// Field-wise initializer
    public init(totalBytes: UInt64, width: Int, height: Int) {
        self.totalBytes = totalBytes
        self.width = width
        self.height = height
    }
}

/// Metadata for a single file (1.1)
public struct FileMeta: Codable, Sendable, Equatable {
    /// File name only, no path; the receiver still sanitizes it — the peer is
    /// not trusted
    public var name: String
    /// Byte count of this file; how the concatenated stream is split
    public var bytes: UInt64

    /// Field-wise initializer
    public init(name: String, bytes: UInt64) {
        self.name = name
        self.bytes = bytes
    }
}

/// File offer metadata: a JSON string carried in clipboard_sync.data (1.1)
public struct FilesOfferMeta: Codable, Sendable, Equatable {
    /// Total length of the raw byte stream that follows, the sum of the file
    /// byte counts; the receiver checks the two agree
    public var totalBytes: UInt64
    /// File list; the stream concatenates them in this order
    public var files: [FileMeta]

    /// Wire key names (matching the Rust serde snake_case)
    enum CodingKeys: String, CodingKey {
        case totalBytes = "total_bytes"
        case files
    }

    /// Field-wise initializer
    public init(totalBytes: UInt64, files: [FileMeta]) {
        self.totalBytes = totalBytes
        self.files = files
    }
}

/// Control channel message
public enum ControlMessage: Sendable, Equatable {
    /// Session handshake (initiator → receiver)
    case hello(version: String, info: PeerInfo)
    /// Handshake reply, carrying the receiver's device info
    case helloAck(version: String, info: PeerInfo)
    /// Pairing request; identities were already exchanged in Hello, so this
    /// frame only states intent
    case pairRequest
    /// Pairing reply: the peer user's decision in the dialog
    case pairResponse(accepted: Bool)
    /// Unpair notification, best effort
    case unpair
    /// Clipboard sync payload; data is byte-for-byte, never trimmed, never
    /// escaped
    case clipboardSync(seq: UInt64, timestampMs: UInt64, contentType: String, data: String)
    /// The blob offer was accepted; the requester may start sending the raw
    /// byte stream (1.1)
    ///
    /// Transaction order, three branches: after the offer the receiver replies
    /// blobAccept (wanted), syncAck (LWW says ours is stale, no transfer
    /// needed) or syncRejected (refused). Once the stream ends the requester
    /// sends [`blobFooter`], and the receiver verifies the hash before
    /// replying with the final outcome.
    case blobAccept
    /// End marker of the raw blob byte stream, carrying the BLAKE3 integrity
    /// checksum over the whole stream (1.1)
    case blobFooter(hash: String)
    /// The sync was accepted and written to the clipboard
    case syncAck
    /// The sync was rejected, with a structured reason code
    case syncRejected(reasonCode: String)
    /// Gracefully close the session
    case bye

    /// Short message type name for logs and error messages; identical to the
    /// type value on the wire
    public var kind: String {
        switch self {
        case .hello: "hello"
        case .helloAck: "hello_ack"
        case .pairRequest: "pair_request"
        case .pairResponse: "pair_response"
        case .unpair: "unpair"
        case .clipboardSync: "clipboard_sync"
        case .blobAccept: "blob_accept"
        case .blobFooter: "blob_footer"
        case .syncAck: "sync_ack"
        case .syncRejected: "sync_rejected"
        case .bye: "bye"
        }
    }
}

extension ControlMessage: Codable {
    /// Wire key names: the tag and every variant's fields share one flattened
    /// container
    private enum CodingKeys: String, CodingKey {
        case type
        case version
        case info
        case accepted
        case seq
        case timestampMs = "timestamp_ms"
        case contentType = "content_type"
        case data
        case hash
        case reasonCode = "reason_code"
    }

    /// Variant discriminant (= the Rust snake_case variant name)
    private enum Kind: String, Codable {
        case hello
        case helloAck = "hello_ack"
        case pairRequest = "pair_request"
        case pairResponse = "pair_response"
        case unpair
        case clipboardSync = "clipboard_sync"
        case blobAccept = "blob_accept"
        case blobFooter = "blob_footer"
        case syncAck = "sync_ack"
        case syncRejected = "sync_rejected"
        case bye
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(Kind.self, forKey: .type) {
        case .hello:
            self = .hello(
                version: try c.decode(String.self, forKey: .version),
                info: try c.decode(PeerInfo.self, forKey: .info))
        case .helloAck:
            self = .helloAck(
                version: try c.decode(String.self, forKey: .version),
                info: try c.decode(PeerInfo.self, forKey: .info))
        case .pairRequest:
            self = .pairRequest
        case .pairResponse:
            self = .pairResponse(accepted: try c.decode(Bool.self, forKey: .accepted))
        case .unpair:
            self = .unpair
        case .clipboardSync:
            self = .clipboardSync(
                seq: try c.decode(UInt64.self, forKey: .seq),
                timestampMs: try c.decode(UInt64.self, forKey: .timestampMs),
                contentType: try c.decode(String.self, forKey: .contentType),
                data: try c.decode(String.self, forKey: .data))
        case .blobAccept:
            self = .blobAccept
        case .blobFooter:
            self = .blobFooter(hash: try c.decode(String.self, forKey: .hash))
        case .syncAck:
            self = .syncAck
        case .syncRejected:
            self = .syncRejected(reasonCode: try c.decode(String.self, forKey: .reasonCode))
        case .bye:
            self = .bye
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .hello(let version, let info):
            try c.encode(Kind.hello, forKey: .type)
            try c.encode(version, forKey: .version)
            try c.encode(info, forKey: .info)
        case .helloAck(let version, let info):
            try c.encode(Kind.helloAck, forKey: .type)
            try c.encode(version, forKey: .version)
            try c.encode(info, forKey: .info)
        case .pairRequest:
            try c.encode(Kind.pairRequest, forKey: .type)
        case .pairResponse(let accepted):
            try c.encode(Kind.pairResponse, forKey: .type)
            try c.encode(accepted, forKey: .accepted)
        case .unpair:
            try c.encode(Kind.unpair, forKey: .type)
        case .clipboardSync(let seq, let timestampMs, let contentType, let data):
            try c.encode(Kind.clipboardSync, forKey: .type)
            try c.encode(seq, forKey: .seq)
            try c.encode(timestampMs, forKey: .timestampMs)
            try c.encode(contentType, forKey: .contentType)
            try c.encode(data, forKey: .data)
        case .blobAccept:
            try c.encode(Kind.blobAccept, forKey: .type)
        case .blobFooter(let hash):
            try c.encode(Kind.blobFooter, forKey: .type)
            try c.encode(hash, forKey: .hash)
        case .syncAck:
            try c.encode(Kind.syncAck, forKey: .type)
        case .syncRejected(let reasonCode):
            try c.encode(Kind.syncRejected, forKey: .type)
            try c.encode(reasonCode, forKey: .reasonCode)
        case .bye:
            try c.encode(Kind.bye, forKey: .type)
        }
    }
}

/// Check the peer's protocol version: the same major counts as compatible
public func isVersionCompatible(_ peerVersion: String) -> Bool {
    majorSegment(peerVersion) == majorSegment(Config.protocolVersion)
}

/// Extract the major segment of a version string
private func majorSegment(_ version: String) -> Substring {
    version.split(separator: ".", maxSplits: 1).first ?? Substring(version)
}
