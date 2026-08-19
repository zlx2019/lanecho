package io.github.zlx2019.lanecho.core.discovery

import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

// UDP discovery packets (PROTOCOL.md §6): one JSON document per datagram, no
// length prefix. The peer IP comes from the UDP source address, never the body

@Serializable
enum class AnnounceKind {
    /** Periodic multicast: I am online. */
    @SerialName("announce")
    ANNOUNCE,

    /** Unicast reply to an announce so a new node sees existing ones instantly. */
    @SerialName("response")
    RESPONSE,

    /** Graceful go-offline. */
    @SerialName("goodbye")
    GOODBYE,
}

@Serializable
data class AnnouncePacket(
    val kind: AnnounceKind,
    val info: PeerInfo,
    @SerialName("tcp_port") val tcpPort: Int,
)
