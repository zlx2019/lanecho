package io.github.zlx2019.lanecho.core.history

import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class HistoryStoreTest {

    @TempDir
    lateinit var dir: Path

    private fun store(limit: Int = 200) = HistoryStore(dir, maxEntries = limit)

    @Test
    fun sameContentBumpsInsteadOfDuplicating() {
        val store = store()
        val first = store.recordText("hello", origin = null, timestampMs = 100)!!
        val bumped = store.recordText("hello", origin = "MacBook", timestampMs = 200)!!
        assertEquals(first.id, bumped.id)
        assertEquals(2, bumped.copyCount)
        assertEquals(200, bumped.lastCopiedAt)
        assertEquals(100, bumped.firstCopiedAt)
        // Remote origin overwrites the label; a later local copy keeps it
        assertEquals("MacBook", bumped.origin)
        val local = store.recordText("hello", origin = null, timestampMs = 300)!!
        assertEquals("MacBook", local.origin)
        assertEquals(1, store.list().size)
    }

    @Test
    fun listOrdersPinnedFirstThenNewest() {
        val store = store()
        store.recordText("old", null, 100)
        val pinned = store.recordText("pinned", null, 50)!!
        store.recordText("new", null, 200)
        store.setPinned(pinned.id, true)
        assertEquals(listOf("pinned", "new", "old"), store.list().map { it.text })
    }

    @Test
    fun evictionSkipsPinnedAndDropsOldest() {
        val store = store(limit = 2)
        val keep = store.recordText("keep", null, 10)!!
        store.setPinned(keep.id, true)
        store.recordText("second", null, 20)
        store.recordText("third", null, 30)
        val texts = store.list().map { it.text }
        assertTrue("keep" in texts, "pinned must survive eviction")
        assertFalse("second" in texts, "oldest unpinned must be evicted")
        assertTrue("third" in texts)
    }

    @Test
    fun imageBlobIsContentAddressedAndCleanedUp() {
        val store = store()
        val png = ByteArray(1000) { it.toByte() }
        val rgba = RgbaImage(10, 10, ByteArray(400) { (it % 7).toByte() })
        val entry = store.recordImage(png, rgba, origin = "MacBook", timestampMs = 100)!!
        assertEquals(EntryKind.IMAGE, entry.kind)
        assertEquals("10×10", entry.preview)
        val blob = store.blobPath(entry.blobHash!!)
        assertTrue(Files.exists(blob))
        assertTrue(png.contentEquals(Files.readAllBytes(blob)), "blob stores the received PNG untouched")
        store.delete(entry.id)
        assertFalse(Files.exists(blob), "orphan blob must be removed with its entry")
    }

    @Test
    fun persistsAcrossReload() {
        val first = store()
        first.recordText("persisted", "MacBook", 100)
        val reloaded = store()
        reloaded.load()
        assertEquals(1, reloaded.list().size)
        assertEquals("persisted", reloaded.list()[0].text)
        assertEquals("MacBook", reloaded.list()[0].origin)
    }

    // Disk shape stays desktop-compatible: camelCase keys, absent optionals
    // omitted entirely (never null)
    @Test
    fun diskShapeIsCamelCaseWithOmittedOptionals() {
        store().recordText("shape", null, 100)
        val text = Files.readString(dir.resolve("history").resolve("index.json"))
        assertTrue(""""contentHash":""" in text)
        assertTrue(""""lastCopiedAt":100""" in text)
        assertFalse(""""origin"""" in text, "absent origin must be omitted: $text")
        assertFalse("content_hash" in text)
    }

    @Test
    fun corruptIndexStartsEmpty() {
        Files.createDirectories(dir.resolve("history"))
        Files.writeString(dir.resolve("history").resolve("index.json"), "{ broken")
        val store = store()
        store.load()
        assertEquals(0, store.list().size)
        assertNotNull(store.recordText("still works", null, 1))
    }

    @Test
    fun oversizedTextIsRefused() {
        assertNull(store().recordText("x".repeat(5 * 1024 * 1024 + 1), null, 1))
    }

    @Test
    fun searchMatchesPreviewAndFullText() {
        val store = store()
        store.recordText("first line\nNEEDLE in the second line", null, 100)
        store.recordText("nothing here", null, 200)
        assertEquals(1, store.search("needle").size)
        assertEquals(2, store.search("").size)
    }

    // CRLF text (exactly what a Windows peer syncs over) must still yield a
    // single-line preview — the Swift grapheme-cluster trap
    @Test
    fun previewHandlesCrlfAndCaps() {
        assertEquals("windows line", previewOf("windows line\r\nsecond"))
        assertEquals("x".repeat(80), previewOf("x".repeat(300)))
        assertEquals("", previewOf(""))
    }
}
