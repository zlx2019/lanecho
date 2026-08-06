// Frame protocol tests, mirroring the Rust protocol.rs test surface.
//
// Extra cross-implementation guard: the "golden JSON" is taken verbatim from
// Rust serde output. Both directions are pinned, Swift decoding what Rust
// produces and Swift's own encoding round-tripping through itself.

import Foundation
import NIOCore
import NIOEmbedded
import Testing

@testable import LanechoKit

/// Builds the peer info used by the tests, matching the same-named Rust helper.
private func peerInfo() -> PeerInfo {
    PeerInfo(
        deviceId: "d1", name: "n1",
        fingerprint: String(repeating: "f", count: 64),
        platform: "macos", osVersion: "macOS 15.3")
}

/// Frame codec round-trip: every message kind decodes back unchanged
/// (mirrors the Rust frame_roundtrip test).
@Test func frameRoundtrip() throws {
    let samples: [ControlMessage] = [
        .hello(version: Config.protocolVersion, info: peerInfo()),
        .helloAck(version: Config.protocolVersion, info: peerInfo()),
        .pairRequest,
        .pairResponse(accepted: true),
        .unpair,
        // Text must survive byte-for-byte: deliberately carries leading and
        // trailing whitespace plus control characters
        .clipboardSync(
            seq: 7, timestampMs: 1_752_000_000_000,
            contentType: ContentType.text,
            data: "  hello\n\t emoji🚀 \0 tail  "),
        .syncAck,
        .syncRejected(reasonCode: ReasonCode.notPaired),
        .bye,
    ]
    for message in samples {
        let frame = try FrameCodec.encode(message)
        #expect(try FrameCodec.decode(frame) == message)
    }
}

/// The data field of a sync message is never altered; byte-for-byte guard.
@Test func syncDataIsByteExact() throws {
    let raw = "  space  \u{7f} text "
    let message = ControlMessage.clipboardSync(
        seq: 0, timestampMs: 0, contentType: ContentType.text, data: raw)
    let decoded = try FrameCodec.decode(FrameCodec.encode(message))
    guard case .clipboardSync(_, _, _, let data) = decoded else {
        Issue.record("expected clipboard_sync")
        return
    }
    #expect(data == raw)
}

/// Golden JSON (verbatim Rust serde output) must decode: direct evidence of
/// cross-implementation interop.
@Test func decodesRustGoldenJSON() throws {
    // Literal JSON lifted from the Rust protocol.rs tests
    let sync = #"{"type":"clipboard_sync","seq":1,"timestamp_ms":2,"content_type":"image/png","data":"x"}"#
    let decoded = try JSONDecoder().decode(ControlMessage.self, from: Data(sync.utf8))
    // An unknown content type must still parse: the peer answers SyncRejected
    // instead of failing the parse and dropping the connection
    #expect(
        decoded
            == .clipboardSync(seq: 1, timestampMs: 2, contentType: "image/png", data: "x"))

    let hello = #"{"type":"hello","version":"1.0","info":{"device_id":"d","name":"n","fingerprint":"f","platform":"linux","os_version":"Ubuntu 24.04"}}"#
    let helloDecoded = try JSONDecoder().decode(ControlMessage.self, from: Data(hello.utf8))
    guard case .hello(let version, let info) = helloDecoded else {
        Issue.record("expected hello")
        return
    }
    #expect(version == "1.0")
    #expect(info.deviceId == "d")
    #expect(info.osVersion == "Ubuntu 24.04")

    // Field-less variants and rejection reasons
    #expect(
        try JSONDecoder().decode(
            ControlMessage.self, from: Data(#"{"type":"bye"}"#.utf8)) == .bye)
    #expect(
        try JSONDecoder().decode(
            ControlMessage.self,
            from: Data(#"{"type":"sync_rejected","reason_code":"too_large"}"#.utf8))
            == .syncRejected(reasonCode: ReasonCode.tooLarge))
}

/// Peer info from an older version (no os_version) must parse, and optional
/// fields that are unset must not be serialized.
@Test func peerInfoOptionalFieldsAreBackwardCompatible() throws {
    let legacy = #"{"device_id":"d","name":"n","fingerprint":"f","platform":"macos"}"#
    let info = try JSONDecoder().decode(PeerInfo.self, from: Data(legacy.utf8))
    #expect(info.osVersion == nil)
    let encoded = String(decoding: try JSONEncoder().encode(info), as: UTF8.self)
    #expect(!encoded.contains("os_version"))
}

/// Decoding an unknown type variant must fail, matching serde's behaviour on
/// unknown variants.
@Test func unknownMessageTypeFailsToDecode() {
    #expect(throws: (any Error).self) {
        try JSONDecoder().decode(
            ControlMessage.self, from: Data(#"{"type":"image_sync","data":"x"}"#.utf8))
    }
}

/// Oversized frames are rejected on both the encode and the decode path.
@Test func oversizedFrameRejected() {
    let huge = ControlMessage.clipboardSync(
        seq: 0, timestampMs: 0, contentType: ContentType.text,
        data: String(repeating: "x", count: Int(Config.maxFrameLen) + 1))
    #expect(throws: ProtocolError.self) { try FrameCodec.encode(huge) }

    // A header declaring an over-limit length is rejected without reading the
    // body
    var bogus = [UInt8]()
    withUnsafeBytes(of: (Config.maxFrameLen + 1).bigEndian) { bogus.append(contentsOf: $0) }
    #expect(throws: ProtocolError.self) { try FrameCodec.decode(bogus) }
}

/// Version compatibility: a matching major is compatible, a different one is
/// rejected.
@Test func versionCompat() {
    #expect(isVersionCompatible("1.0"))
    #expect(isVersionCompatible("1.9"))
    #expect(!isVersionCompatible("2.0"))
}

/// NIO pipeline framing: a half frame split across TCP segments waits for the
/// rest, and several frames arriving in one batch are emitted one by one.
@Test func nioPipelineReassemblesFrames() throws {
    let raw = RawInboundState()
    let channel = EmbeddedChannel(handlers: [ByteToMessageHandler(FrameDecoder(raw: raw))])
    let first = try FrameCodec.encode(.pairRequest)
    let second = try FrameCodec.encode(.pairResponse(accepted: false))

    // Half a frame: only the first 3 bytes, so nothing comes out
    try channel.writeInbound(ByteBuffer(bytes: Array(first[0..<3])))
    #expect(try channel.readInbound(as: WireMessage.self) == nil)
    // Feed the remainder plus a whole second frame: two messages come out
    var rest = ByteBuffer(bytes: Array(first[3...]))
    rest.writeBytes(second)
    try channel.writeInbound(rest)
    guard case .frame(let one) = try channel.readInbound(as: WireMessage.self),
        case .frame(let two) = try channel.readInbound(as: WireMessage.self)
    else {
        Issue.record("Two frame messages should be emitted")
        return
    }
    #expect(one == .pairRequest)
    #expect(two == .pairResponse(accepted: false))
    _ = try? channel.finish()
}

/// NIO pipeline raw-stream mode (1.1 blob): writing BlobAccept switches the
/// pipeline into raw mode, exactly n bytes are handed up as `.raw` without
/// frame parsing, and frame mode resumes once the count is reached. Raw bytes
/// arriving in the same batch as the following frame must still split
/// correctly.
@Test func nioPipelineSwitchesToRawStreamMode() throws {
    let (decoder, encoder) = makeFrameHandlers()
    let channel = EmbeddedChannel(handlers: [decoder, encoder])

    // The receiving side writes "accepted, expect a 5-byte raw stream"
    try channel.writeOutbound(WireMessage.blobAcceptExpectingRaw(bytes: 5))
    var accepted = try #require(try channel.readOutbound(as: ByteBuffer.self))
    let acceptedBytes = accepted.readBytes(length: accepted.readableBytes) ?? []
    #expect(try FrameCodec.decode(acceptedBytes) == .blobAccept, "The wire format must contain a valid BlobAccept frame")

    // The peer's 5 raw bytes and the footer frame arrive in one batch: the
    // first 5 bytes go up as raw, then frame parsing resumes immediately
    let payload: [UInt8] = [0xDE, 0xAD, 0xBE, 0xEF, 0x99]
    var inbound = ByteBuffer(bytes: payload)
    inbound.writeBytes(try FrameCodec.encode(.blobFooter(hash: "h")))
    try channel.writeInbound(inbound)

    guard case .raw(var chunk) = try channel.readInbound(as: WireMessage.self) else {
        Issue.record("A raw stream segment must be surfaced as raw")
        return
    }
    #expect(chunk.readBytes(length: chunk.readableBytes) == payload)
    guard case .frame(let footer) = try channel.readInbound(as: WireMessage.self) else {
        Issue.record("Frame parsing must resume after the raw stream reaches its length")
        return
    }
    #expect(footer == .blobFooter(hash: "h"))
    _ = try? channel.finish()
}

/// Discovery packets: round-trip, decode of the Rust golden JSON, and silent
/// rejection of packets that are not lanecho's.
@Test func announcePacketRoundtripAndGolden() throws {
    let packet = AnnouncePacket(kind: .announce, info: peerInfo(), tcpPort: 42524)
    let data = try packet.encoded()
    #expect(AnnouncePacket.decoded(from: data) == packet)

    let golden = #"{"kind":"goodbye","info":{"device_id":"d","name":"n","fingerprint":"f","platform":"windows"},"tcp_port":42524}"#
    let decoded = try #require(AnnouncePacket.decoded(from: Data(golden.utf8)))
    #expect(decoded.kind == .goodbye)
    #expect(decoded.tcpPort == 42524)
    #expect(decoded.info.platform == "windows")

    #expect(AnnouncePacket.decoded(from: Data("not json".utf8)) == nil)
}
