package io.github.zlx2019.lanecho.core.history

import io.github.zlx2019.lanecho.core.blake3.Blake3
import io.github.zlx2019.lanecho.core.util.Log
import io.github.zlx2019.lanecho.core.util.atomicWrite
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import java.nio.file.Files
import java.nio.file.Path
import java.util.UUID

// History engine: index.json + content-addressed PNG blobs. The on-disk shape
// mirrors the desktop HistoryEntry (camelCase, absent optionals omitted) —
// the phone never shares a data directory with the desktop, but keeping the
// format identical keeps tooling and golden tests portable.

/** Decoded RGBA pixels; the hash baseline for images (PROTOCOL §9, never PNG bytes). */
class RgbaImage(val width: Int, val height: Int, val pixels: ByteArray)

/** Platform seam for PNG decoding: ImageIO on JVM tests, BitmapFactory on Android. */
interface ImageCodec {
    /** Decode PNG bytes into non-premultiplied RGBA, or null when undecodable. */
    fun decodeRgba(png: ByteArray): RgbaImage?
}

object EntryKind {
    const val TEXT = "text"
    const val IMAGE = "image"
}

@Serializable
data class HistoryEntry(
    val id: String,
    val kind: String,
    val text: String? = null,
    val blobHash: String? = null,
    val preview: String = "",
    val contentHash: String = "",
    val firstCopiedAt: Long = 0,
    val lastCopiedAt: Long = 0,
    val copyCount: Int = 1,
    /** null = this device; a device name when a remote sync wrote it in. */
    val origin: String? = null,
    val pinned: Boolean = false,
)

private val indexJson = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
}

/** Maximum text entry size (5 MB, the desktop cap). */
private const val MAX_TEXT_BYTES = 5 * 1024 * 1024

/**
 * All mutations run under the store lock and persist synchronously (single
 * writer; phone-scale lists make batching unnecessary). Serialization failure
 * never reaches the disk; writes are atomic.
 */
class HistoryStore(
    private val dataDir: Path,
    @Volatile var maxEntries: Int = 200,
) {
    private val lock = Any()
    private val entries = mutableListOf<HistoryEntry>()
    private val indexPath = dataDir.resolve("history").resolve("index.json")
    private val blobsDir = dataDir.resolve("history").resolve("blobs")

    fun load() {
        if (!Files.exists(indexPath)) return
        val loaded = try {
            indexJson.decodeFromString(ListSerializer(HistoryEntry.serializer()), Files.readAllBytes(indexPath).decodeToString())
        } catch (e: Exception) {
            Log.warn("history index unreadable, starting empty: ${e.message}")
            return
        }
        synchronized(lock) {
            entries.clear()
            entries.addAll(loaded)
        }
    }

    /** Newest first, pinned on top (the panel order of the desktop clients). */
    fun list(): List<HistoryEntry> = synchronized(lock) {
        entries.sortedWith(compareByDescending<HistoryEntry> { it.pinned }.thenByDescending { it.lastCopiedAt })
    }

    fun find(id: String): HistoryEntry? = synchronized(lock) { entries.firstOrNull { it.id == id } }

    /** Search over preview and full text, lowercase containment (desktop semantics). */
    fun search(query: String): List<HistoryEntry> {
        if (query.isBlank()) return list()
        val needle = query.lowercase()
        return list().filter { entry ->
            needle in entry.preview.lowercase() || (entry.text?.lowercase()?.contains(needle) ?: false)
        }
    }

    /** Record a text item; same content bumps count/time instead of duplicating. */
    fun recordText(text: String, origin: String?, timestampMs: Long): HistoryEntry? {
        if (text.isEmpty() || text.encodeToByteArray().size > MAX_TEXT_BYTES) return null
        val hash = Blake3.hashHex(text.encodeToByteArray())
        return record(hash, origin, timestampMs) {
            HistoryEntry(
                id = UUID.randomUUID().toString(),
                kind = EntryKind.TEXT,
                text = text,
                preview = previewOf(text),
                contentHash = hash,
                firstCopiedAt = timestampMs,
                lastCopiedAt = timestampMs,
                origin = origin,
            )
        }
    }

    /**
     * Record an image: the blob stores the received PNG bytes untouched
     * (restore stays original resolution), while the dedup hash comes from
     * the decoded RGBA pixels.
     */
    fun recordImage(png: ByteArray, rgba: RgbaImage, origin: String?, timestampMs: Long): HistoryEntry? {
        val hash = Blake3.hashHex(rgba.pixels)
        return record(hash, origin, timestampMs) {
            val blobHash = Blake3.hashHex(png)
            Files.createDirectories(blobsDir)
            val blobPath = blobsDir.resolve("$blobHash.png")
            if (!Files.exists(blobPath)) atomicWrite(blobPath, png)
            HistoryEntry(
                id = UUID.randomUUID().toString(),
                kind = EntryKind.IMAGE,
                blobHash = blobHash,
                preview = "${rgba.width}×${rgba.height}",
                contentHash = hash,
                firstCopiedAt = timestampMs,
                lastCopiedAt = timestampMs,
                origin = origin,
            )
        }
    }

    private fun record(contentHash: String, origin: String?, timestampMs: Long, create: () -> HistoryEntry): HistoryEntry {
        val result: HistoryEntry
        synchronized(lock) {
            val index = entries.indexOfFirst { it.contentHash == contentHash }
            result = if (index >= 0) {
                // Bump: remote origin overwrites the label, a local re-copy
                // (origin=null) keeps whatever was there
                val existing = entries[index]
                val bumped = existing.copy(
                    lastCopiedAt = maxOf(existing.lastCopiedAt, timestampMs),
                    copyCount = existing.copyCount + 1,
                    origin = origin ?: existing.origin,
                )
                entries[index] = bumped
                bumped
            } else {
                val fresh = create()
                entries.add(fresh)
                evictLocked()
                fresh
            }
            persistLocked()
        }
        return result
    }

    fun setPinned(id: String, pinned: Boolean): Boolean = mutate(id) { it.copy(pinned = pinned) }

    fun delete(id: String): Boolean {
        var removed: HistoryEntry?
        synchronized(lock) {
            val index = entries.indexOfFirst { it.id == id }
            if (index < 0) return false
            removed = entries.removeAt(index)
            persistLocked()
            removed?.let(::deleteBlobIfOrphanLocked)
        }
        return true
    }

    fun clear() {
        synchronized(lock) {
            entries.clear()
            persistLocked()
            runCatching {
                if (Files.exists(blobsDir)) {
                    Files.list(blobsDir).use { stream -> stream.forEach { Files.deleteIfExists(it) } }
                }
            }
        }
    }

    fun blobPath(blobHash: String): Path = blobsDir.resolve("$blobHash.png")

    /** Approximate disk usage (index + blobs) in bytes. */
    fun diskUsage(): Long = synchronized(lock) {
        var total = runCatching { Files.size(indexPath) }.getOrDefault(0L)
        runCatching {
            if (Files.exists(blobsDir)) {
                Files.list(blobsDir).use { stream ->
                    stream.forEach { total += runCatching { Files.size(it) }.getOrDefault(0L) }
                }
            }
        }
        total
    }

    private fun mutate(id: String, transform: (HistoryEntry) -> HistoryEntry): Boolean {
        synchronized(lock) {
            val index = entries.indexOfFirst { it.id == id }
            if (index < 0) return false
            entries[index] = transform(entries[index])
            persistLocked()
        }
        return true
    }

    // Evict the oldest unpinned entries beyond the cap; pinned never evicts
    private fun evictLocked() {
        while (entries.size > maxEntries) {
            val victim = entries.filter { !it.pinned }.minByOrNull { it.lastCopiedAt } ?: return
            entries.remove(victim)
            deleteBlobIfOrphanLocked(victim)
        }
    }

    private fun deleteBlobIfOrphanLocked(entry: HistoryEntry) {
        val blobHash = entry.blobHash ?: return
        if (entries.none { it.blobHash == blobHash }) {
            runCatching { Files.deleteIfExists(blobsDir.resolve("$blobHash.png")) }
        }
    }

    private fun persistLocked() {
        val body = try {
            indexJson.encodeToString(ListSerializer(HistoryEntry.serializer()), entries.toList())
        } catch (e: Exception) {
            Log.warn("history serialization failed, skipping persist: ${e.message}")
            return
        }
        atomicWrite(indexPath, body.encodeToByteArray())
    }
}

/**
 * First line, capped at 80 chars. lineSequence splits on CRLF as well —
 * CRLF text synced from Windows is exactly the case that broke the Swift
 * first-line scan (grapheme-cluster "\r\n" trap).
 */
internal fun previewOf(text: String): String {
    val firstLine = text.lineSequence().firstOrNull() ?: ""
    return if (firstLine.length <= 80) firstLine else firstLine.take(80)
}
