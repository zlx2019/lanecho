// Frame codec: a 4-byte big-endian length prefix plus a JSON body, capped at
// 1 MiB.
//
// Two layers:
// - FrameCodec: pure-function encode/decode, reused by tests and by
//   pre-encoding — a broadcast shares one frame across every target, the same
//   arrangement as encode_frame/Arc<[u8]> on the Rust side
// - FrameDecoder/FrameEncoder: NIO pipeline handlers, the transport form used
//   by session transactions
//
// Since 1.1 a pipeline message is a [`WireMessage`] — either a frame or a raw
// stream chunk: a blob transaction splices a run of **contiguous raw bytes
// with no frame header** into the frame sequence. The Rust side simply reads
// and writes the TCP stream, but the NIO pipeline has to switch explicitly:
// when the receiver writes `.blobAcceptExpectingRaw(n)`, the encoder puts that
// connection's decoder into raw mode (the next n bytes are passed up as `.raw`
// chunks with no frame parsing) and it switches back once the count is met.
// Ordering makes this safe: the peer only sends the stream after receiving
// BlobAccept, and the mode switch happens while encoding that write — on the
// same event loop, strictly before any raw byte can arrive.

import Foundation
import NIOCore

/// Protocol layer error
public enum ProtocolError: Error, Equatable {
    /// Frame length exceeds the cap; the parameter is the declared length
    case frameTooLarge(UInt32)
    /// Incompatible version: the peer's major differs
    case versionMismatch(peer: String)
    /// Received a message that does not fit the current session state
    case unexpectedMessage(expected: String, got: String)
}

/// Pure-function frame codec
public enum FrameCodec {
    /// Pre-encode one frame: the length prefix and JSON body as contiguous
    /// bytes
    ///
    /// A broadcast sends the same frame to every target, so it is encoded once
    /// and shared rather than serialized per peer.
    public static func encode(_ message: ControlMessage) throws -> [UInt8] {
        let body = try JSONEncoder().encode(message)
        guard body.count <= Int(Config.maxFrameLen) else {
            throw ProtocolError.frameTooLarge(UInt32(clamping: body.count))
        }
        var frame = [UInt8]()
        frame.reserveCapacity(4 + body.count)
        withUnsafeBytes(of: UInt32(body.count).bigEndian) { frame.append(contentsOf: $0) }
        frame.append(contentsOf: body)
        return frame
    }

    /// Decode one complete frame's bytes (for tests; streaming decode goes
    /// through [`FrameDecoder`])
    public static func decode(_ frame: [UInt8]) throws -> ControlMessage {
        guard frame.count >= 4 else {
            throw ProtocolError.frameTooLarge(0)
        }
        let len = frame[0..<4].reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        guard len <= Config.maxFrameLen, frame.count == 4 + Int(len) else {
            throw ProtocolError.frameTooLarge(len)
        }
        return try JSONDecoder().decode(ControlMessage.self, from: Data(frame[4...]))
    }
}

/// Pipeline message: a frame, or a raw stream chunk (the stream segment of a
/// blob transaction)
public enum WireMessage: Sendable {
    /// One control message frame
    case frame(ControlMessage)
    /// Raw stream chunk; inbound it is what the decoder's raw mode produces,
    /// outbound it is written as-is with no framing
    case raw(ByteBuffer)
    /// Outbound only: encode a BlobAccept frame and put this connection's
    /// decoder into raw mode, after which exactly `bytes` bytes are passed up
    /// as `.raw`; used only by the receiving side of a blob
    case blobAcceptExpectingRaw(bytes: Int)
}

/// Per-connection raw mode state: written by the encoder, consumed by the
/// decoder
///
/// Both handlers only touch it on this connection's event loop, so no lock is
/// needed; one instance is created per connection at wiring time and is never
/// shared across connections.
final class RawInboundState {
    /// Raw stream bytes still to consume (0 = frame mode)
    var remaining = 0
}

/// NIO inbound frame decoder, wrapped into the pipeline by
/// ByteToMessageHandler
///
/// An oversized frame errors out and drops the connection before its body is
/// read, guarding against memory blowup, same as the Rust read_frame. In raw
/// mode chunks are passed up as they arrive: a chunk shorter than `remaining`
/// is legal and the consumer counts up to the total; chunks bear no relation
/// to file boundaries, the receiver splits them itself.
public final class FrameDecoder: ByteToMessageDecoder {
    public typealias InboundOut = WireMessage

    /// One decoder instance is reused per connection; building one per frame
    /// is pure waste
    private let json = JSONDecoder()
    /// Raw mode switch, set by the encoder when it writes BlobAccept
    private let raw: RawInboundState

    init(raw: RawInboundState) {
        self.raw = raw
    }

    public func decode(context: ChannelHandlerContext, buffer: inout ByteBuffer) throws
        -> DecodingState
    {
        if raw.remaining > 0 {
            let take = min(raw.remaining, buffer.readableBytes)
            guard take > 0, let chunk = buffer.readSlice(length: take) else {
                return .needMoreData
            }
            raw.remaining -= take
            context.fireChannelRead(wrapInboundOut(.raw(chunk)))
            return .continue
        }
        guard let len: UInt32 = buffer.getInteger(at: buffer.readerIndex) else {
            return .needMoreData
        }
        guard len <= Config.maxFrameLen else {
            throw ProtocolError.frameTooLarge(len)
        }
        guard buffer.readableBytes >= 4 + Int(len) else {
            return .needMoreData
        }
        buffer.moveReaderIndex(forwardBy: 4)
        guard let body = buffer.readBytes(length: Int(len)) else {
            return .needMoreData
        }
        let message = try json.decode(ControlMessage.self, from: Data(body))
        context.fireChannelRead(wrapInboundOut(.frame(message)))
        return .continue
    }

    public func decodeLast(
        context: ChannelHandlerContext, buffer: inout ByteBuffer, seenEOF: Bool
    ) throws -> DecodingState {
        // A half frame or half raw stream is discarded on EOF (the peer
        // disconnected); not an error
        .needMoreData
    }
}

/// NIO outbound frame encoder
public final class FrameEncoder: MessageToByteEncoder {
    public typealias OutboundIn = WireMessage

    /// Raw stream state shared with this connection's decoder
    private let raw: RawInboundState

    init(raw: RawInboundState) {
        self.raw = raw
    }

    public func encode(data: WireMessage, out: inout ByteBuffer) throws {
        switch data {
        case .frame(let message):
            out.writeBytes(try FrameCodec.encode(message))
        case .raw(var chunk):
            out.writeBuffer(&chunk)
        case .blobAcceptExpectingRaw(let bytes):
            out.writeBytes(try FrameCodec.encode(.blobAccept))
            raw.remaining = bytes
        }
    }
}

/// Build the frame codec handler pair for one connection, sharing a single
/// raw stream state
func makeFrameHandlers() -> (ByteToMessageHandler<FrameDecoder>, MessageToByteHandler<FrameEncoder>)
{
    let raw = RawInboundState()
    return (
        ByteToMessageHandler(FrameDecoder(raw: raw)),
        MessageToByteHandler(FrameEncoder(raw: raw))
    )
}
