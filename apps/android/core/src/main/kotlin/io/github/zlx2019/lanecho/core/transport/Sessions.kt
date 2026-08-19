package io.github.zlx2019.lanecho.core.transport

import io.github.zlx2019.lanecho.core.Config
import io.github.zlx2019.lanecho.core.PROTOCOL_VERSION
import io.github.zlx2019.lanecho.core.identity.DeviceIdentity
import io.github.zlx2019.lanecho.core.protocol.ControlMessage
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.protocol.readFrame
import io.github.zlx2019.lanecho.core.protocol.versionCompatible
import io.github.zlx2019.lanecho.core.protocol.writeFrame
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import javax.net.ssl.SSLSocket

// Outbound transactions (PROTOCOL.md §5): dial, transact, leave. Every
// transaction opens a fresh TLS connection, passes the Hello gate and ends
// with Bye + drain-to-EOF (JSSE has no TCP half-close; the receiver closes
// upon Bye, so draining to its close_notify keeps in-flight frames intact)

/** Failure taxonomy of one dial transaction. */
sealed class SessionException(message: String) : Exception(message) {
    class PeerUnreachable : SessionException("no candidate address accepted a TLS connection")
    class Timeout(stage: String) : SessionException("timed out waiting for $stage")
    class Rejected(val reasonCode: String) : SessionException("peer refused: $reasonCode")
    class PairRefused : SessionException("peer user refused the pairing")
    class FingerprintMismatch : SessionException("declared fingerprint does not match the pinned certificate")
    class VersionMismatch(peer: String) : SessionException("incompatible protocol version: $peer")
    class Unexpected(expected: String, got: String) : SessionException("expected $expected, got $got")
}

/** Minimal dial target: candidate addresses plus the pinned fingerprint. */
data class DialTarget(
    val addresses: List<String>,
    val port: Int,
    val fingerprint: String,
)

object Sessions {

    /** Pairing transaction; returns the remote info once its user accepts. */
    fun pair(identity: DeviceIdentity, localInfo: PeerInfo, target: DialTarget): PeerInfo =
        transact(identity, localInfo, target) { socket, remote ->
            writeFrame(socket.outputStream, ControlMessage.PairRequest)
            // A human is in the loop on the other side, hence the long timeout
            socket.soTimeout = Config.PAIR_DECISION_TIMEOUT_MS.toInt()
            when (val reply = readReply(socket, "pair_response")) {
                is ControlMessage.PairResponse ->
                    if (reply.accepted) remote else throw SessionException.PairRefused()
                else -> throw SessionException.Unexpected("pair_response", reply.kind)
            }
        }

    /** Text sync transaction: deliver one clipboard text and require the verdict. */
    fun syncText(
        identity: DeviceIdentity,
        localInfo: PeerInfo,
        target: DialTarget,
        seq: Long,
        timestampMs: Long,
        text: String,
    ) {
        transact(identity, localInfo, target) { socket, _ ->
            writeFrame(
                socket.outputStream,
                ControlMessage.ClipboardSync(
                    seq = seq,
                    timestampMs = timestampMs,
                    contentType = io.github.zlx2019.lanecho.core.protocol.ContentType.TEXT,
                    data = text,
                ),
            )
            when (val reply = readReply(socket, "sync_ack")) {
                ControlMessage.SyncAck -> Unit
                is ControlMessage.SyncRejected -> throw SessionException.Rejected(reply.reasonCode)
                else -> throw SessionException.Unexpected("sync_ack", reply.kind)
            }
        }
    }

    /** Unpair notification (best effort; the security boundary is receiver-side). */
    fun unpair(identity: DeviceIdentity, localInfo: PeerInfo, target: DialTarget) {
        transact(identity, localInfo, target) { socket, _ ->
            writeFrame(socket.outputStream, ControlMessage.Unpair)
        }
    }

    /**
     * Liveness probe (PROTOCOL.md §8): TLS handshake + explicit fingerprint
     * comparison, hanging up right after — a bare TCP connect is no proof of
     * life. Returns true only when the certificate matches [DialTarget.fingerprint].
     */
    fun probe(identity: DeviceIdentity, address: String, port: Int, expectedFingerprint: String): Boolean {
        val context = TlsContexts.clientContext(identity, pinnedFingerprint = null)
        return try {
            val raw = Socket()
            raw.tcpNoDelay = true
            raw.soTimeout = Config.PEER_PROBE_TIMEOUT_MS.toInt()
            raw.connect(InetSocketAddress(address, port), Config.PEER_PROBE_TIMEOUT_MS.toInt())
            val socket = context.socketFactory.createSocket(raw, address, port, true) as SSLSocket
            TlsContexts.restrict(socket)
            socket.useClientMode = true
            socket.startHandshake()
            val alive = TlsContexts.peerFingerprint(socket.session) == expectedFingerprint
            socket.close()
            alive
        } catch (_: Exception) {
            false
        }
    }

    private fun <T> transact(
        identity: DeviceIdentity,
        localInfo: PeerInfo,
        target: DialTarget,
        transaction: (SSLSocket, PeerInfo) -> T,
    ): T {
        val socket = connect(identity, target)
        try {
            val remote = handshakeOut(socket, localInfo, target.fingerprint)
            val result = transaction(socket, remote)
            runCatching { writeFrame(socket.outputStream, ControlMessage.Bye) }
            drainToEof(socket)
            return result
        } finally {
            runCatching { socket.close() }
        }
    }

    // Dial each candidate address in turn; TLS pinned strictly to the target
    // fingerprint (a stale address pointing at another device fails the pin)
    private fun connect(identity: DeviceIdentity, target: DialTarget): SSLSocket {
        val context = TlsContexts.clientContext(identity, target.fingerprint)
        for (address in target.addresses) {
            try {
                val raw = Socket()
                raw.tcpNoDelay = true
                raw.connect(InetSocketAddress(address, target.port), Config.CONNECT_TIMEOUT_MS.toInt())
                val socket = context.socketFactory.createSocket(raw, address, target.port, true) as SSLSocket
                TlsContexts.restrict(socket)
                socket.useClientMode = true
                socket.soTimeout = Config.REPLY_TIMEOUT_MS.toInt()
                socket.startHandshake()
                return socket
            } catch (_: Exception) {
                // Try the next candidate address
            }
        }
        throw SessionException.PeerUnreachable()
    }

    // Hello → HelloAck, checking version compatibility and that the declared
    // fingerprint equals the pinned one (blocks impersonation). The peer may
    // refuse during the handshake itself — surface its reason verbatim
    private fun handshakeOut(socket: SSLSocket, localInfo: PeerInfo, expectedFingerprint: String): PeerInfo {
        writeFrame(socket.outputStream, ControlMessage.Hello(PROTOCOL_VERSION, localInfo))
        return when (val reply = readReply(socket, "hello_ack")) {
            is ControlMessage.HelloAck -> {
                if (!versionCompatible(reply.version)) throw SessionException.VersionMismatch(reply.version)
                if (reply.info.fingerprint != expectedFingerprint) throw SessionException.FingerprintMismatch()
                reply.info
            }
            is ControlMessage.SyncRejected -> throw SessionException.Rejected(reply.reasonCode)
            else -> throw SessionException.Unexpected("hello_ack", reply.kind)
        }
    }

    private fun readReply(socket: SSLSocket, stage: String): ControlMessage = try {
        readFrame(socket.inputStream)
    } catch (_: SocketTimeoutException) {
        throw SessionException.Timeout(stage)
    }

    // Read until the peer's close_notify lands (EOF), bounded; this replaces
    // the shutdown-then-drain of the Rust side, which JSSE cannot express
    private fun drainToEof(socket: SSLSocket) {
        runCatching {
            socket.soTimeout = Config.GRACEFUL_CLOSE_TIMEOUT_MS.toInt()
            val sink = ByteArray(256)
            while (socket.inputStream.read(sink) != -1) {
                // Discard: transactions are over, only Bye/close_notify remain
            }
        }
    }
}
