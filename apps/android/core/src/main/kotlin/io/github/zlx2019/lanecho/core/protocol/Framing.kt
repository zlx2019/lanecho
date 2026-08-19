package io.github.zlx2019.lanecho.core.protocol

import java.io.DataInputStream
import java.io.EOFException
import java.io.InputStream
import java.io.OutputStream

// Frame format: 4-byte big-endian length prefix + JSON body (PROTOCOL.md §4).
// Blocking-stream implementation: connection counts are single digits and
// transactions are short, so plain SSLSocket streams on IO dispatchers carry
// the whole transport (no NIO)

/** Maximum length of a single frame (1 MiB); only JSON frames, never blob streams. */
const val MAX_FRAME_LEN = 1024 * 1024

sealed class ProtocolException(message: String) : Exception(message) {
    class FrameTooLarge(length: Long) : ProtocolException("frame of $length bytes exceeds $MAX_FRAME_LEN")
    class Codec(detail: String) : ProtocolException("message codec failure: $detail")
    class VersionMismatch(peer: String, local: String) :
        ProtocolException("incompatible protocol version: peer $peer, local $local")
    class Unexpected(expected: String, got: String) :
        ProtocolException("unexpected message: expected $expected, got $got")
}

/** Pre-encode one frame into contiguous bytes (length prefix + body). */
fun encodeFrame(message: ControlMessage): ByteArray {
    val body = try {
        wireJson.encodeToString(ControlMessage.serializer(), message).encodeToByteArray()
    } catch (e: Exception) {
        throw ProtocolException.Codec(e.message ?: e.toString())
    }
    if (body.size > MAX_FRAME_LEN) throw ProtocolException.FrameTooLarge(body.size.toLong())
    val frame = ByteArray(4 + body.size)
    frame[0] = (body.size ushr 24).toByte()
    frame[1] = (body.size ushr 16).toByte()
    frame[2] = (body.size ushr 8).toByte()
    frame[3] = body.size.toByte()
    body.copyInto(frame, 4)
    return frame
}

/** Encode and write one frame, then flush. */
fun writeFrame(out: OutputStream, message: ControlMessage) {
    out.write(encodeFrame(message))
    out.flush()
}

/** Read one frame; an oversized length errors out before its body is read. */
fun readFrame(input: InputStream): ControlMessage {
    val data = DataInputStream(input)
    val length = data.readInt().toLong() and 0xFFFF_FFFFL
    if (length > MAX_FRAME_LEN) throw ProtocolException.FrameTooLarge(length)
    val body = ByteArray(length.toInt())
    data.readFully(body)
    return try {
        wireJson.decodeFromString(ControlMessage.serializer(), body.decodeToString())
    } catch (e: Exception) {
        throw ProtocolException.Codec(e.message ?: e.toString())
    }
}

/** Read one frame, mapping a clean EOF before the length prefix to null. */
fun readFrameOrNull(input: InputStream): ControlMessage? = try {
    readFrame(input)
} catch (_: EOFException) {
    null
}
