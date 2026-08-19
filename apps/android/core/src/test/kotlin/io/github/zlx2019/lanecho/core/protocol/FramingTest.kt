package io.github.zlx2019.lanecho.core.protocol

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull

class FramingTest {

    private val info = PeerInfo("d1", "n1", "f".repeat(64), "android", "Android 16")

    @Test
    fun framesRoundTripThroughStreams() {
        val samples = listOf(
            ControlMessage.Hello("1.1", info),
            ControlMessage.PairRequest,
            ControlMessage.PairResponse(accepted = true),
            ControlMessage.ClipboardSync(7, 1_752_000_000_000, ContentType.TEXT, "  你好\n\t emoji🚀 \u0000 尾巴  "),
            ControlMessage.SyncRejected(ReasonCode.NOT_PAIRED),
            ControlMessage.Bye,
        )
        val buffer = ByteArrayOutputStream()
        for (message in samples) writeFrame(buffer, message)
        val input = ByteArrayInputStream(buffer.toByteArray())
        for (message in samples) assertEquals(message, readFrame(input))
        assertNull(readFrameOrNull(input))
    }

    @Test
    fun framePrefixIsBigEndianLength() {
        val frame = encodeFrame(ControlMessage.Bye)
        val body = """{"type":"bye"}""".encodeToByteArray()
        assertEquals(4 + body.size, frame.size)
        val length = ((frame[0].toInt() and 0xFF) shl 24) or ((frame[1].toInt() and 0xFF) shl 16) or
            ((frame[2].toInt() and 0xFF) shl 8) or (frame[3].toInt() and 0xFF)
        assertEquals(body.size, length)
        assertEquals(body.decodeToString(), frame.copyOfRange(4, frame.size).decodeToString())
    }

    @Test
    fun oversizedMessageRefusedWhileEncoding() {
        val oversized = ControlMessage.ClipboardSync(0, 0, ContentType.TEXT, "x".repeat(MAX_FRAME_LEN + 1))
        assertFailsWith<ProtocolException.FrameTooLarge> { encodeFrame(oversized) }
    }

    // A bogus length prefix over the limit must be refused before any body
    // bytes are read (guards against a malicious oversized frame)
    @Test
    fun oversizedFrameRefusedBeforeReadingBody() {
        val length = MAX_FRAME_LEN + 1
        val prefix = byteArrayOf(
            (length ushr 24).toByte(), (length ushr 16).toByte(), (length ushr 8).toByte(), length.toByte(),
        )
        assertFailsWith<ProtocolException.FrameTooLarge> { readFrame(ByteArrayInputStream(prefix)) }
    }

    @Test
    fun garbageBodyIsCodecError() {
        val body = "not json".encodeToByteArray()
        val frame = ByteArray(4 + body.size)
        frame[3] = body.size.toByte()
        body.copyInto(frame, 4)
        assertFailsWith<ProtocolException.Codec> { readFrame(ByteArrayInputStream(frame)) }
    }
}
