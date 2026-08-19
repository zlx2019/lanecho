package io.github.zlx2019.lanecho.core.discovery

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.StandardProtocolFamily
import java.net.StandardSocketOptions
import java.nio.ByteBuffer
import java.nio.channels.DatagramChannel

// CI runners often have no working multicast loopback (the same trap the
// Swift suite guards against): every multicast test must be gated on this
// probe or it fails as a false negative only on CI. The probe mirrors the
// production channel exactly (INET-family channels, per-interface join/send)
object MulticastAvailability {
    const val SKIP_REASON = "multicast loopback unavailable in this environment"

    val available: Boolean by lazy { probe() }

    private fun probe(): Boolean = try {
        val group = InetAddress.getByName("224.0.0.169")
        val receiver = DatagramChannel.open(StandardProtocolFamily.INET)
        receiver.setOption(StandardSocketOptions.SO_REUSEADDR, true)
        runCatching { receiver.setOption(StandardSocketOptions.SO_REUSEPORT, true) }
        receiver.bind(InetSocketAddress(0))
        val port = (receiver.localAddress as InetSocketAddress).port
        val interfaces = NetworkInterface.getNetworkInterfaces().toList().filter { nic ->
            nic.isUp && !nic.isLoopback && nic.supportsMulticast() &&
                nic.interfaceAddresses.any { it.address is java.net.Inet4Address }
        }
        interfaces.forEach { runCatching { receiver.join(group, it) } }
        receiver.socket().soTimeout = 300

        val sender = DatagramChannel.open(StandardProtocolFamily.INET)
        sender.setOption(StandardSocketOptions.IP_MULTICAST_LOOP, true)
        sender.bind(null)
        val payload = ByteBuffer.wrap("lanecho-multicast-probe".encodeToByteArray())
        var heard = false
        val buffer = ByteBuffer.allocate(256)
        val deadline = System.currentTimeMillis() + 1_500
        while (!heard && System.currentTimeMillis() < deadline) {
            for (nic in interfaces) {
                runCatching {
                    sender.setOption(StandardSocketOptions.IP_MULTICAST_IF, nic)
                    payload.rewind()
                    sender.send(payload, InetSocketAddress(group, port))
                }
            }
            Thread.sleep(50)
            receiver.configureBlocking(false)
            buffer.clear()
            heard = receiver.receive(buffer) != null
        }
        sender.close()
        receiver.close()
        heard
    } catch (_: Exception) {
        false
    }
}
