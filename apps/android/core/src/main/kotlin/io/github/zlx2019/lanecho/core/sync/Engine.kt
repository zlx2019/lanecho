package io.github.zlx2019.lanecho.core.sync

import io.github.zlx2019.lanecho.core.Config
import io.github.zlx2019.lanecho.core.DEFAULT_DISCOVERY_PORT
import io.github.zlx2019.lanecho.core.blake3.Blake3
import io.github.zlx2019.lanecho.core.discovery.Peer
import io.github.zlx2019.lanecho.core.discovery.PeerEvent
import io.github.zlx2019.lanecho.core.discovery.PeerRegistry
import io.github.zlx2019.lanecho.core.discovery.UdpDiscoveryChannel
import io.github.zlx2019.lanecho.core.history.HistoryEntry
import io.github.zlx2019.lanecho.core.history.HistoryStore
import io.github.zlx2019.lanecho.core.history.ImageCodec
import io.github.zlx2019.lanecho.core.identity.DeviceIdentity
import io.github.zlx2019.lanecho.core.protocol.ContentType
import io.github.zlx2019.lanecho.core.protocol.ImageOfferMeta
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.protocol.ReasonCode
import io.github.zlx2019.lanecho.core.transport.DialTarget
import io.github.zlx2019.lanecho.core.transport.OfferDecision
import io.github.zlx2019.lanecho.core.transport.ReceiverCallbacks
import io.github.zlx2019.lanecho.core.transport.SessionException
import io.github.zlx2019.lanecho.core.transport.Sessions
import io.github.zlx2019.lanecho.core.transport.SyncReceiver
import io.github.zlx2019.lanecho.core.util.Log
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/** Engine → UI events; callbacks arrive on engine/connection threads. */
interface EngineListener {
    /** The online peer list changed. */
    fun onPeersChanged() {}

    /** History changed (new entry, bump, delete, clear). */
    fun onHistoryChanged() {}

    /** A remote sync landed (already recorded); the UI decides banner vs auto-write. */
    fun onRemoteApplied(entry: HistoryEntry, fromName: String) {}

    /** An inbound pairing request awaits a user verdict via [Engine.resolvePair]. */
    fun onPairRequest(remote: PeerInfo) {}
}

/**
 * The mobile engine: wires identity, discovery, transport, pairing, history
 * and settings into the foreground-online lifecycle (start on foreground,
 * goodbye + stop on background). The phone never watches its clipboard —
 * outbound traffic happens only through explicit calls.
 */
class Engine(
    dataDir: Path,
    private val imageCodec: ImageCodec,
    /** Fallback display name when identity.json has none (e.g. device model). */
    private val defaultName: String,
    private val osVersion: String?,
) : ReceiverCallbacks {
    val identityDir: Path = dataDir
    var identity: DeviceIdentity = DeviceIdentity.loadOrCreate(dataDir)
        private set
    val settings = SettingsStore(dataDir.resolve("settings.json"))
    val paired = PairedStore(dataDir.resolve("paired.json"))
    val history = HistoryStore(dataDir)
    val registry = PeerRegistry(selfFingerprint = identity.fingerprint, onEvent = ::onPeerEvent)

    private val listeners = CopyOnWriteArrayList<EngineListener>()
    private val pendingPairs = ConcurrentHashMap<String, CompletableFuture<Boolean>>()
    private val sequence = AtomicLong(0)

    /** LWW baseline: the moment of the newest content this device has seen. */
    @Volatile
    private var baselineMs: Long = 0

    /** Content hash of the newest applied/broadcast content (echo guard for DA3). */
    @Volatile
    private var baselineHash: String? = null

    private var receiver: SyncReceiver? = null
    private var udp: UdpDiscoveryChannel? = null

    fun addListener(listener: EngineListener) = listeners.add(listener)
    fun removeListener(listener: EngineListener) = listeners.remove(listener)

    fun localInfo(): PeerInfo = PeerInfo(
        deviceId = identity.deviceId,
        name = identity.displayName ?: defaultName,
        fingerprint = identity.fingerprint,
        platform = "android",
        osVersion = osVersion,
    )

    /** Load stores; call once before the first [goOnline]. */
    fun initialize() {
        settings.load()
        paired.load()
        history.maxEntries = settings.current.historyLimit
        history.load()
    }

    /** Foreground: bind the receiver, join discovery, announce. */
    @Synchronized
    fun goOnline() {
        if (receiver != null) return
        val port = settings.current.port
        val syncReceiver = SyncReceiver(identity, ::localInfo, this)
        val boundPort = try {
            syncReceiver.start(port)
        } catch (e: Exception) {
            Log.warn("receiver bind failed on port $port: ${e.message}")
            return
        }
        receiver = syncReceiver
        val channel = UdpDiscoveryChannel(
            selfFingerprint = identity.fingerprint,
            localInfo = ::localInfo,
            tcpPort = { boundPort },
            port = DEFAULT_DISCOVERY_PORT,
            registry = registry,
        )
        channel.start()
        udp = channel
    }

    /** Background: goodbye, unbind, drop transient peers. */
    @Synchronized
    fun goOffline() {
        udp?.stop()
        udp = null
        receiver?.stop()
        receiver = null
        // The registry snapshot is session-scoped: next foreground starts fresh
        registry.snapshot().forEach { registry.goodbye(it.info.fingerprint) }
    }

    val isOnline: Boolean get() = receiver != null

    // ---- Outbound actions (explicit; never automatic) ----

    /**
     * DA3 opportunistic upstream: broadcast only when the clipboard content
     * is genuinely new — the echo guard stops re-broadcasting what a remote
     * just handed us or what we just sent. Explicit actions (restore, share)
     * go through [broadcastText] instead, unguarded.
     */
    fun broadcastTextIfNew(text: String, timestampMs: Long): Int {
        val hash = Blake3.hashHex(text.encodeToByteArray())
        if (hash == baselineHash) return 0
        return broadcastText(text, timestampMs)
    }

    /**
     * Broadcast text to every paired online peer (restore-from-history and
     * share-sheet semantics: an explicit action always broadcasts, like the
     * desktop "restore counts as a copy"). Returns the number of peers acked.
     */
    fun broadcastText(text: String, timestampMs: Long): Int {
        val hash = Blake3.hashHex(text.encodeToByteArray())
        if (text.encodeToByteArray().size > Config.MAX_SYNC_TEXT_BYTES) {
            Log.warn("text over the sync cap, recording locally only")
            history.recordText(text, origin = null, timestampMs = timestampMs)
            notifyHistory()
            return 0
        }
        baselineMs = maxOf(baselineMs, timestampMs)
        baselineHash = hash
        history.recordText(text, origin = null, timestampMs = timestampMs)
        notifyHistory()
        val targets = onlinePairedPeers()
        var delivered = 0
        val seq = sequence.incrementAndGet()
        for (peer in targets) {
            try {
                Sessions.syncText(identity, localInfo(), target(peer), seq, timestampMs, text)
                delivered++
            } catch (e: SessionException.PeerUnreachable) {
                registry.dialFailed(peer.info.fingerprint)
            } catch (e: SessionException) {
                Log.warn("sync to ${peer.info.name} refused: ${e.message}")
            }
        }
        return delivered
    }

    /** Dial a pairing request; blocks up to 300s for the remote user. */
    fun pairWith(fingerprint: String): PeerInfo {
        val peer = registry.find(fingerprint) ?: throw SessionException.PeerUnreachable()
        val remote = Sessions.pair(identity, localInfo(), target(peer))
        paired.upsert(
            PairedPeer(
                fingerprint = remote.fingerprint,
                deviceId = remote.deviceId,
                name = remote.name,
                pairedAtMs = System.currentTimeMillis(),
            ),
        )
        notifyPeers()
        return remote
    }

    fun unpairWith(fingerprint: String) {
        // Best effort on the wire; the local removal is what matters
        registry.find(fingerprint)?.let { peer ->
            runCatching { Sessions.unpair(identity, localInfo(), target(peer)) }
        }
        paired.remove(fingerprint)
        notifyPeers()
    }

    /** UI verdict for a pending inbound pairing request. */
    fun resolvePair(fingerprint: String, accepted: Boolean) {
        pendingPairs.remove(fingerprint)?.complete(accepted)
    }

    fun rename(name: String?) {
        identity = identity.withDisplayName(identityDir, name)
        udp?.announceNow()
        notifyPeers()
    }

    fun onlinePairedPeers(): List<Peer> =
        registry.snapshot().filter { paired.isPaired(it.info.fingerprint) }

    private fun target(peer: Peer) = DialTarget(peer.addresses, peer.port, peer.info.fingerprint)

    // ---- ReceiverCallbacks (inbound; connection threads) ----

    override fun decidePair(remote: PeerInfo): Boolean {
        val future = CompletableFuture<Boolean>()
        pendingPairs[remote.fingerprint] = future
        listeners.forEach { it.onPairRequest(remote) }
        val accepted = try {
            future.get(Config.PAIR_DECISION_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        } catch (_: Exception) {
            false
        } finally {
            pendingPairs.remove(remote.fingerprint)
        }
        if (accepted) {
            paired.upsert(
                PairedPeer(
                    fingerprint = remote.fingerprint,
                    deviceId = remote.deviceId,
                    name = remote.name,
                    pairedAtMs = System.currentTimeMillis(),
                ),
            )
            notifyPeers()
        }
        return accepted
    }

    // Inbound check chain, order fixed (PROTOCOL §5): direction toggle →
    // paired → type toggle → size → LWW (stale still acks)
    override fun acceptSync(remote: PeerInfo, timestampMs: Long, contentType: String, data: String): String? {
        if (contentType != ContentType.TEXT) return ReasonCode.UNSUPPORTED_TYPE
        if (!settings.current.receiveText) return ReasonCode.DISABLED
        if (!paired.isPaired(remote.fingerprint)) return ReasonCode.NOT_PAIRED
        if (data.encodeToByteArray().size > Config.MAX_SYNC_TEXT_BYTES) return ReasonCode.TOO_LARGE
        if (timestampMs <= baselineMs) {
            Log.info("LWW judged stale from ${remote.name} (${timestampMs} <= ${baselineMs}), acked and ignored")
            return null
        }
        baselineMs = timestampMs
        baselineHash = Blake3.hashHex(data.encodeToByteArray())
        val entry = history.recordText(data, origin = remote.name, timestampMs = timestampMs)
        notifyHistory()
        entry?.let { recorded -> listeners.forEach { it.onRemoteApplied(recorded, remote.name) } }
        return null
    }

    override fun decideImageOffer(remote: PeerInfo, timestampMs: Long, meta: ImageOfferMeta): OfferDecision {
        if (!settings.current.receiveImages) return OfferDecision.Reject(ReasonCode.UNSUPPORTED_TYPE)
        if (!paired.isPaired(remote.fingerprint)) return OfferDecision.Reject(ReasonCode.NOT_PAIRED)
        if (meta.totalBytes <= 0 || meta.totalBytes > Config.MAX_SYNC_IMAGE_BYTES) {
            return OfferDecision.Reject(ReasonCode.TOO_LARGE)
        }
        if (timestampMs <= baselineMs) return OfferDecision.Stale
        return OfferDecision.Accept
    }

    override fun finishImage(remote: PeerInfo, timestampMs: Long, png: ByteArray): String? {
        // The dedup/LWW hash baseline is the decoded RGBA, never PNG bytes
        val rgba = imageCodec.decodeRgba(png) ?: return ReasonCode.UNSUPPORTED_TYPE
        if (timestampMs > baselineMs) {
            baselineMs = timestampMs
            baselineHash = Blake3.hashHex(rgba.pixels)
        }
        val entry = history.recordImage(png, rgba, origin = remote.name, timestampMs = timestampMs)
        notifyHistory()
        entry?.let { recorded -> listeners.forEach { it.onRemoteApplied(recorded, remote.name) } }
        return null
    }

    override fun onUnpair(remote: PeerInfo) {
        paired.remove(remote.fingerprint)
        notifyPeers()
    }

    // ---- Internals ----

    private fun onPeerEvent(@Suppress("UNUSED_PARAMETER") event: PeerEvent) = notifyPeers()

    private fun notifyPeers() = listeners.forEach { it.onPeersChanged() }
    private fun notifyHistory() = listeners.forEach { it.onHistoryChanged() }

    /** Mark content applied to the clipboard by the UI (echo guard for DA3). */
    fun noteAppliedToClipboard(entry: HistoryEntry) {
        baselineHash = entry.contentHash
    }
}
