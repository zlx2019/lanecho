package io.github.zlx2019.lanecho.core.protocol

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

// golden_wire.jsonl is genuine Rust serde output (captured from
// lanecho-core), one frame body per line. Every line must decode, re-encode
// and decode again to an equal value — the wire-shape contract of the third
// protocol implementation
class MessagesGoldenTest {

    private fun goldenLines(): List<String> =
        checkNotNull(javaClass.getResourceAsStream("/golden_wire.jsonl")) { "golden resource missing" }
            .readBytes().decodeToString().trim().lines()

    @Test
    fun everyGoldenLineRoundTrips() {
        val lines = goldenLines()
        assertEquals(13, lines.size)
        for (line in lines) {
            val decoded = wireJson.decodeFromString(ControlMessage.serializer(), line)
            val reEncoded = wireJson.encodeToString(ControlMessage.serializer(), decoded)
            val decodedAgain = wireJson.decodeFromString(ControlMessage.serializer(), reEncoded)
            assertEquals(decoded, decodedAgain, "unstable roundtrip: $line")
        }
    }

    @Test
    fun helloCarriesFullPeerInfo() {
        val hello = wireJson.decodeFromString(ControlMessage.serializer(), goldenLines()[0])
        assertIs<ControlMessage.Hello>(hello)
        assertEquals("1.1", hello.version)
        assertEquals("d1", hello.info.deviceId)
        assertEquals("n1", hello.info.name)
        assertEquals("f".repeat(64), hello.info.fingerprint)
        assertEquals("macos", hello.info.platform)
        assertEquals("macOS 15.3", hello.info.osVersion)
    }

    // Legacy PeerInfo without os_version must decode to null, and our own
    // encoding must omit the absent field entirely (never emit "os_version":null)
    @Test
    fun legacyPeerInfoOmitsOsVersionBothWays()  {
        val helloAck = wireJson.decodeFromString(ControlMessage.serializer(), goldenLines()[1])
        assertIs<ControlMessage.HelloAck>(helloAck)
        assertNull(helloAck.info.osVersion)
        val encoded = wireJson.encodeToString(ControlMessage.serializer(), helloAck)
        assertFalse("os_version" in encoded)
    }

    // Clipboard text is byte exact: whitespace, control chars, NUL and emoji
    // survive serde's escaping untouched
    @Test
    fun clipboardSyncDataIsByteExact() {
        val sync = wireJson.decodeFromString(ControlMessage.serializer(), goldenLines()[5])
        assertIs<ControlMessage.ClipboardSync>(sync)
        assertEquals(7, sync.seq)
        assertEquals(1_752_000_000_000, sync.timestampMs)
        assertEquals(ContentType.TEXT, sync.contentType)
        assertEquals("  你好\n\t emoji🚀 \u0000 尾巴  ", sync.data)
    }

    @Test
    fun blobOfferMetasParseFromDataField() {
        val image = wireJson.decodeFromString(ControlMessage.serializer(), goldenLines()[6])
        assertIs<ControlMessage.ClipboardSync>(image)
        assertEquals(ContentType.IMAGE, image.contentType)
        val imageMeta = wireJson.decodeFromString(ImageOfferMeta.serializer(), image.data)
        assertEquals(123456, imageMeta.totalBytes)
        assertEquals(640, imageMeta.width)
        assertEquals(480, imageMeta.height)

        val files = wireJson.decodeFromString(ControlMessage.serializer(), goldenLines()[7])
        assertIs<ControlMessage.ClipboardSync>(files)
        val filesMeta = wireJson.decodeFromString(FilesOfferMeta.serializer(), files.data)
        assertEquals(30, filesMeta.totalBytes)
        assertEquals(listOf(FileMeta("a.txt", 10), FileMeta("b.bin", 20)), filesMeta.files)
    }

    @Test
    fun fieldlessVariantsEncodeAsBareTag() {
        for ((message, tag) in listOf(
            ControlMessage.PairRequest to "pair_request",
            ControlMessage.Unpair to "unpair",
            ControlMessage.BlobAccept to "blob_accept",
            ControlMessage.SyncAck to "sync_ack",
            ControlMessage.Bye to "bye",
        )) {
            assertEquals("""{"type":"$tag"}""", wireJson.encodeToString(ControlMessage.serializer(), message))
            assertEquals(tag, message.kind)
        }
    }

    // Evolution rule §10: an unknown content_type is an ordinary string (the
    // refusal path), while an unknown message type is a decode error (new
    // message kinds only appear after the peer opted in)
    @Test
    fun unknownContentTypeParsesUnknownMessageTypeFails() {
        val json = """{"type":"clipboard_sync","seq":1,"timestamp_ms":2,"content_type":"video/mp4","data":"x"}"""
        val sync = wireJson.decodeFromString(ControlMessage.serializer(), json)
        assertIs<ControlMessage.ClipboardSync>(sync)
        assertEquals("video/mp4", sync.contentType)

        assertFails {
            wireJson.decodeFromString(ControlMessage.serializer(), """{"type":"telepathy_sync"}""")
        }
    }

    // Unknown extra fields inside a known message must be ignored (serde
    // default-field evolution)
    @Test
    fun unknownFieldsAreIgnored() {
        val json = """{"type":"pair_response","accepted":false,"future_field":{"nested":1}}"""
        val response = wireJson.decodeFromString(ControlMessage.serializer(), json)
        assertIs<ControlMessage.PairResponse>(response)
        assertFalse(response.accepted)
    }

    @Test
    fun versionCompatibilityIsMajorOnly() {
        assertTrue(versionCompatible("1.0"))
        assertTrue(versionCompatible("1.9"))
        assertFalse(versionCompatible("2.0"))
    }
}
