package io.github.zlx2019.lanecho.core.sync

import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class PairedStoreTest {

    @TempDir
    lateinit var dir: Path

    private fun peer(id: String) = PairedPeer(
        fingerprint = id.repeat(64).take(64),
        deviceId = "device-$id",
        name = "peer-$id",
        pairedAtMs = 1_752_000_000_000,
    )

    @Test
    fun upsertPersistsAndReloads() {
        val path = dir.resolve("paired.json")
        val store = PairedStore(path)
        store.upsert(peer("a"))
        store.upsert(peer("b"))
        assertTrue(store.isPaired(peer("a").fingerprint))

        val reloaded = PairedStore(path)
        reloaded.load()
        assertEquals(2, reloaded.all().size)
        assertEquals(peer("a"), reloaded.all()[0])
    }

    // The on-disk shape is a JSON array with snake_case fields (PROTOCOL.md
    // §11); pinned so desktop-side tooling could read the same format
    @Test
    fun diskShapeIsSnakeCaseArray() {
        val path = dir.resolve("paired.json")
        PairedStore(path).upsert(peer("a"))
        val text = Files.readString(path)
        assertTrue(text.startsWith("["))
        assertTrue(""""device_id":"device-a"""" in text)
        assertTrue(""""paired_at_ms":1752000000000""" in text)
        assertFalse("deviceId" in text)
    }

    @Test
    fun removeUnknownIsNoop() {
        val store = PairedStore(dir.resolve("paired.json"))
        store.upsert(peer("a"))
        assertFalse(store.remove("missing"))
        assertTrue(store.remove(peer("a").fingerprint))
        assertFalse(store.isPaired(peer("a").fingerprint))
    }

    @Test
    fun corruptFileStartsEmptyWithoutCrashing() {
        val path = dir.resolve("paired.json")
        Files.writeString(path, "{ not an array")
        val store = PairedStore(path)
        store.load()
        assertEquals(0, store.all().size)
        // And the store keeps working afterwards
        store.upsert(peer("a"))
        assertTrue(store.isPaired(peer("a").fingerprint))
    }
}
