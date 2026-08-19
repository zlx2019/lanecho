package io.github.zlx2019.lanecho.core.discovery

import io.github.zlx2019.lanecho.core.Config
import io.github.zlx2019.lanecho.core.protocol.PeerInfo

/** An online node with its candidate addresses (non-loopback IPv4 first). */
data class Peer(
    val info: PeerInfo,
    val addresses: List<String>,
    val port: Int,
)

/** Node up/down event; Down carries the certificate fingerprint. */
sealed interface PeerEvent {
    data class Up(val peer: Peer) : PeerEvent
    data class Down(val fingerprint: String) : PeerEvent
}

/**
 * Discovery registry: merges UDP and mDNS sightings into one peer table.
 * Pure logic with injected time so every rule is testable.
 *
 * Liveness rules (PROTOCOL.md §8):
 * - addresses only ever accumulate (multi-NIC / DHCP churn leaves stale ones);
 * - UDP-sourced liveness expires after [Config.PEER_TIMEOUT_MS];
 * - the mDNS-alive flag is cleared only by an explicit removal event, never by
 *   time (mDNS caches linger ~2min systemically);
 * - goodbye takes a node down instantly.
 *
 * The phone does not run the 30s probe loop (short-session node); a failed
 * dial marks the peer offline instead (see the engine layer).
 */
class PeerRegistry(
    private val selfFingerprint: String,
    private val onEvent: (PeerEvent) -> Unit,
) {
    private class Entry(
        var info: PeerInfo,
        val addresses: LinkedHashSet<String>,
        var port: Int,
        var lastUdpMs: Long,
        var mdnsAlive: Boolean,
    )

    private val lock = Any()
    private val entries = LinkedHashMap<String, Entry>()

    /** A UDP announce/response sighting (source address from the datagram). */
    fun seenUdp(info: PeerInfo, address: String, tcpPort: Int, nowMs: Long) =
        upsert(info, listOf(address), tcpPort, nowMs, viaMdns = false)

    /** An mDNS resolution (all resolved addresses at once). */
    fun seenMdns(info: PeerInfo, addresses: List<String>, tcpPort: Int, nowMs: Long) =
        upsert(info, addresses, tcpPort, nowMs, viaMdns = true)

    private fun upsert(info: PeerInfo, addresses: List<String>, tcpPort: Int, nowMs: Long, viaMdns: Boolean) {
        if (info.fingerprint == selfFingerprint) return
        val usable = addresses.filter(::usableAddress)
        var event: PeerEvent.Up?
        synchronized(lock) {
            val entry = entries.getOrPut(info.fingerprint) {
                Entry(info, LinkedHashSet(), tcpPort, lastUdpMs = 0, mdnsAlive = false)
            }
            val before = snapshotLocked(entry)
            entry.info = info
            entry.addresses.addAll(usable)
            entry.port = tcpPort
            if (viaMdns) entry.mdnsAlive = true else entry.lastUdpMs = nowMs
            val after = snapshotLocked(entry)
            // Only surface a change (fresh node, rename, new address, port move)
            event = if (before != after) PeerEvent.Up(after) else null
        }
        event?.let(onEvent)
    }

    /** Graceful goodbye: instant removal. */
    fun goodbye(fingerprint: String) {
        val removed = synchronized(lock) { entries.remove(fingerprint) != null }
        if (removed) onEvent(PeerEvent.Down(fingerprint))
    }

    /** mDNS removal event (service gone / TTL expiry): clears the alive flag. */
    fun mdnsRemoved(fingerprint: String, nowMs: Long) {
        var down = false
        synchronized(lock) {
            val entry = entries[fingerprint] ?: return
            entry.mdnsAlive = false
            // With no live source left the node goes down right away
            if (nowMs - entry.lastUdpMs > Config.PEER_TIMEOUT_MS) {
                entries.remove(fingerprint)
                down = true
            }
        }
        if (down) onEvent(PeerEvent.Down(fingerprint))
    }

    /** A dial found nobody home: demote the peer (engine-side liveness fallback). */
    fun dialFailed(fingerprint: String) {
        val removed = synchronized(lock) {
            val entry = entries[fingerprint] ?: return
            // mDNS-alive peers survive one failed dial (the record may lag);
            // UDP-only peers are dropped so the list stays honest
            if (entry.mdnsAlive) false else entries.remove(fingerprint) != null
        }
        if (removed) onEvent(PeerEvent.Down(fingerprint))
    }

    /** Time-based sweep: expire UDP-only peers; mDNS-alive ones are never timed out. */
    fun sweep(nowMs: Long) {
        val downs = mutableListOf<String>()
        synchronized(lock) {
            val iterator = entries.iterator()
            while (iterator.hasNext()) {
                val entry = iterator.next().value
                if (!entry.mdnsAlive && nowMs - entry.lastUdpMs > Config.PEER_TIMEOUT_MS) {
                    downs.add(entry.info.fingerprint)
                    iterator.remove()
                }
            }
        }
        downs.forEach { onEvent(PeerEvent.Down(it)) }
    }

    fun snapshot(): List<Peer> = synchronized(lock) { entries.values.map(::snapshotLocked) }

    fun find(fingerprint: String): Peer? =
        synchronized(lock) { entries[fingerprint]?.let(::snapshotLocked) }

    private fun snapshotLocked(entry: Entry): Peer = Peer(
        info = entry.info,
        // Non-loopback IPv4 first, the dial order of PROTOCOL.md §7
        addresses = entry.addresses.sortedBy { address ->
            when {
                ":" in address -> 2
                address.startsWith("127.") -> 1
                else -> 0
            }
        },
        port = entry.port,
    )

    // IPv6 link-local without a scope id cannot be dialed (PROTOCOL.md §7)
    private fun usableAddress(address: String): Boolean =
        !(address.startsWith("fe80:") && "%" !in address)
}
