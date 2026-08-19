package io.github.zlx2019.lanecho.core.discovery

import io.github.zlx2019.lanecho.core.Config
import io.github.zlx2019.lanecho.core.MULTICAST_GROUP
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.protocol.wireJson
import io.github.zlx2019.lanecho.core.util.Log
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.StandardProtocolFamily
import java.net.StandardSocketOptions
import java.nio.ByteBuffer
import java.nio.channels.DatagramChannel
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

// UDP multicast discovery channel (PROTOCOL.md §6): 5s announce heartbeat,
// unicast response to newcomers, multicast goodbye on the way out.
//
// Sockets are DatagramChannel.open(INET) on purpose: the default JVM socket
// on macOS is a dual-stack IPv6 one whose IPv4 multicast membership silently
// misbehaves — the classic reason Java multicast "does not work" while a
// Python probe on the same box succeeds. Joins and sends go per interface so
// multi-NIC hosts cover every segment. On Android the caller must hold a
// WifiManager.MulticastLock while this channel runs.
class UdpDiscoveryChannel(
    private val selfFingerprint: String,
    /** Provider, not a snapshot: renames must reach the next heartbeat. */
    private val localInfo: () -> PeerInfo,
    private val tcpPort: () -> Int,
    private val port: Int,
    private val registry: PeerRegistry,
    /** Passive (incognito) mode receives only and never sends. */
    private val passive: Boolean = false,
) {
    private val running = AtomicBoolean(false)
    private var receiver: DatagramChannel? = null
    private var sender: DatagramChannel? = null
    private val sendLock = Any()
    private val group: InetAddress = InetAddress.getByName(MULTICAST_GROUP)
    private val lastSweepMs = AtomicLong(0)

    fun start() {
        check(running.compareAndSet(false, true)) { "channel already running" }
        val receiver = DatagramChannel.open(StandardProtocolFamily.INET)
        receiver.setOption(StandardSocketOptions.SO_REUSEADDR, true)
        try {
            receiver.setOption(StandardSocketOptions.SO_REUSEPORT, true)
        } catch (_: Exception) {
            // Not supported everywhere (older Android); coexistence only
            // matters on dev machines and CI, which do support it
        }
        receiver.bind(InetSocketAddress(port))
        var joined = 0
        for (nic in eligibleInterfaces()) {
            try {
                receiver.join(group, nic)
                joined++
            } catch (_: Exception) {
                // Interfaces without IPv4 or with odd flags: skip
            }
        }
        if (joined == 0) Log.warn("multicast join succeeded on no interface, discovery will be deaf")
        this.receiver = receiver

        val sender = DatagramChannel.open(StandardProtocolFamily.INET)
        sender.setOption(StandardSocketOptions.IP_MULTICAST_LOOP, true)
        sender.bind(null)
        this.sender = sender

        Thread({ receiveLoop(receiver) }, "lanecho-udp-recv").apply { isDaemon = true }.start()
        if (!passive) {
            Thread({ announceLoop() }, "lanecho-udp-announce").apply { isDaemon = true }.start()
        }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        if (!passive) multicast(AnnounceKind.GOODBYE)
        runCatching { receiver?.close() }
        runCatching { sender?.close() }
        receiver = null
        sender = null
    }

    /** Force one immediate announce (join, wake-up, rename). */
    fun announceNow() {
        if (!passive && running.get()) multicast(AnnounceKind.ANNOUNCE)
    }

    private fun eligibleInterfaces(): List<NetworkInterface> =
        NetworkInterface.getNetworkInterfaces().toList().filter { nic ->
            nic.isUp && !nic.isLoopback && nic.supportsMulticast() &&
                nic.interfaceAddresses.any { it.address is java.net.Inet4Address }
        }

    private fun receiveLoop(receiver: DatagramChannel) {
        val buffer = ByteBuffer.allocate(64 * 1024)
        while (running.get()) {
            buffer.clear()
            val source = try {
                receiver.receive(buffer) as? InetSocketAddress ?: continue
            } catch (_: Exception) {
                if (!running.get()) return
                continue
            }
            buffer.flip()
            val bytes = ByteArray(buffer.remaining())
            buffer.get(bytes)
            val packet = try {
                wireJson.decodeFromString(AnnouncePacket.serializer(), bytes.decodeToString())
            } catch (_: Exception) {
                continue // Not a lanecho packet
            }
            if (packet.info.fingerprint == selfFingerprint) continue
            val sourceAddress = source.address.hostAddress ?: continue
            val now = System.currentTimeMillis()
            when (packet.kind) {
                AnnounceKind.ANNOUNCE -> {
                    registry.seenUdp(packet.info, sourceAddress, packet.tcpPort, now)
                    // Unicast back so the newcomer sees us right away
                    if (!passive) send(AnnounceKind.RESPONSE) { bytes2, sender ->
                        sender.send(ByteBuffer.wrap(bytes2), source)
                    }
                }
                AnnounceKind.RESPONSE -> registry.seenUdp(packet.info, sourceAddress, packet.tcpPort, now)
                AnnounceKind.GOODBYE -> registry.goodbye(packet.info.fingerprint)
            }
            maybeSweep(now)
        }
    }

    private fun announceLoop() {
        // Announce immediately on join, then heartbeat
        multicast(AnnounceKind.ANNOUNCE)
        while (running.get()) {
            Thread.sleep(Config.HEARTBEAT_INTERVAL_MS)
            if (!running.get()) return
            multicast(AnnounceKind.ANNOUNCE)
            maybeSweep(System.currentTimeMillis())
        }
    }

    // Sweep piggybacks on traffic and heartbeats, bounded to once per second
    private fun maybeSweep(nowMs: Long) {
        val last = lastSweepMs.get()
        if (nowMs - last >= 1_000 && lastSweepMs.compareAndSet(last, nowMs)) {
            registry.sweep(nowMs)
        }
    }

    // Multicast one packet out of every eligible interface (multi-NIC hosts
    // must reach every segment); IP_MULTICAST_IF selects the egress
    private fun multicast(kind: AnnounceKind) {
        send(kind) { bytes, sender ->
            val target = InetSocketAddress(group, port)
            var sent = false
            for (nic in eligibleInterfaces()) {
                try {
                    sender.setOption(StandardSocketOptions.IP_MULTICAST_IF, nic)
                    sender.send(ByteBuffer.wrap(bytes), target)
                    sent = true
                } catch (_: Exception) {
                    // Interface may have gone down between enumeration and send
                }
            }
            if (!sent) runCatching { sender.send(ByteBuffer.wrap(bytes), target) }
        }
    }

    private fun send(kind: AnnounceKind, transmit: (ByteArray, DatagramChannel) -> Unit) {
        val sender = this.sender ?: return
        val packet = AnnouncePacket(kind, localInfo(), tcpPort())
        val bytes = try {
            wireJson.encodeToString(AnnouncePacket.serializer(), packet).encodeToByteArray()
        } catch (e: Exception) {
            Log.warn("discovery packet encode failed: ${e.message}")
            return
        }
        synchronized(sendLock) {
            runCatching { transmit(bytes, sender) }
        }
    }
}
