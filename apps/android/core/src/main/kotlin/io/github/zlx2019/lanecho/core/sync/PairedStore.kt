package io.github.zlx2019.lanecho.core.sync

import io.github.zlx2019.lanecho.core.util.Log
import io.github.zlx2019.lanecho.core.util.atomicWrite
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import java.nio.file.Files
import java.nio.file.Path

/** One pairing record; the fingerprint is the key (paired.json, PROTOCOL.md §11). */
@Serializable
data class PairedPeer(
    val fingerprint: String,
    @SerialName("device_id") val deviceId: String,
    /** Display name at pairing time; display only, renames do not affect pairing. */
    val name: String,
    @SerialName("paired_at_ms") val pairedAtMs: Long,
)

private val storeJson = Json { ignoreUnknownKeys = true }

/**
 * The pairing set: an in-memory table persisted as a JSON array. All writes
 * happen under the store lock with the freshest snapshot (single writer).
 */
class PairedStore(private val path: Path) {
    private val lock = Any()
    private val peers = LinkedHashMap<String, PairedPeer>()

    fun load() {
        if (!Files.exists(path)) return
        val loaded = try {
            storeJson.decodeFromString(ListSerializer(PairedPeer.serializer()), Files.readString(path))
        } catch (e: Exception) {
            // A corrupt pairing table must not block startup: peers can re-pair
            Log.warn("paired.json unreadable, starting with an empty pairing set: ${e.message}")
            return
        }
        synchronized(lock) {
            peers.clear()
            for (peer in loaded) peers[peer.fingerprint] = peer
        }
    }

    fun isPaired(fingerprint: String): Boolean = synchronized(lock) { fingerprint in peers }

    fun all(): List<PairedPeer> = synchronized(lock) { peers.values.toList() }

    fun upsert(peer: PairedPeer) {
        synchronized(lock) {
            peers[peer.fingerprint] = peer
            persistLocked()
        }
    }

    fun remove(fingerprint: String): Boolean = synchronized(lock) {
        val removed = peers.remove(fingerprint) != null
        if (removed) persistLocked()
        removed
    }

    // Serialization failure must never reach the disk (an empty write would
    // wipe the table); atomicWrite keeps a crash from truncating it
    private fun persistLocked() {
        val body = try {
            storeJson.encodeToString(ListSerializer(PairedPeer.serializer()), peers.values.toList())
        } catch (e: Exception) {
            Log.warn("paired.json serialization failed, skipping persist: ${e.message}")
            return
        }
        atomicWrite(path, body.encodeToByteArray())
    }
}
