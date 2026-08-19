package io.github.zlx2019.lanecho.core.transport

import io.github.zlx2019.lanecho.core.Config
import io.github.zlx2019.lanecho.core.PROTOCOL_VERSION
import io.github.zlx2019.lanecho.core.blake3.Blake3
import io.github.zlx2019.lanecho.core.blake3.toHex
import io.github.zlx2019.lanecho.core.identity.DeviceIdentity
import io.github.zlx2019.lanecho.core.protocol.ContentType
import io.github.zlx2019.lanecho.core.protocol.ControlMessage
import io.github.zlx2019.lanecho.core.protocol.ImageOfferMeta
import io.github.zlx2019.lanecho.core.protocol.PeerInfo
import io.github.zlx2019.lanecho.core.protocol.ReasonCode
import io.github.zlx2019.lanecho.core.protocol.readFrame
import io.github.zlx2019.lanecho.core.protocol.versionCompatible
import io.github.zlx2019.lanecho.core.protocol.wireJson
import io.github.zlx2019.lanecho.core.protocol.writeFrame
import io.github.zlx2019.lanecho.core.util.Log
import java.io.InputStream
import java.util.concurrent.Semaphore
import java.util.concurrent.atomic.AtomicBoolean
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket

/** Receiver-side verdict on a blob offer (mirrors the Rust OfferDecision). */
sealed interface OfferDecision {
    /** Transfer wanted. */
    data object Accept : OfferDecision

    /** LWW says stale: answer SyncAck without any transfer. */
    data object Stale : OfferDecision

    /** Refused with a reason code. */
    data class Reject(val reasonCode: String) : OfferDecision
}

/**
 * Engine decision seams, injected so the transport stays policy-free.
 * All callbacks run on the connection's own thread.
 */
interface ReceiverCallbacks {
    /** Pairing verdict; may block for the human decision (the dialer waits 300s). */
    fun decidePair(remote: PeerInfo): Boolean

    /** Text (or unknown-type) sync verdict: null = accept, else a reason code. */
    fun acceptSync(remote: PeerInfo, timestampMs: Long, contentType: String, data: String): String?

    /** Image offer verdict; the full check chain runs before any bytes flow. */
    fun decideImageOffer(remote: PeerInfo, timestampMs: Long, meta: ImageOfferMeta): OfferDecision

    /** Landing of a verified image blob: null = accept, else a reason code. */
    fun finishImage(remote: PeerInfo, timestampMs: Long, png: ByteArray): String?

    /** Unpair notification (no reply frame). */
    fun onUnpair(remote: PeerInfo)
}

/**
 * Inbound accept loop: TLS + Hello gate under a deadline, then the transaction
 * loop (PROTOCOL.md §5). Thread-per-connection under a hard concurrency cap —
 * anything beyond it is refused outright so slow-loris cannot exhaust fds.
 */
class SyncReceiver(
    private val identity: DeviceIdentity,
    /** Provider, not a snapshot: hot renames must show up in HelloAck. */
    private val localInfo: () -> PeerInfo,
    private val callbacks: ReceiverCallbacks,
) {
    private val running = AtomicBoolean(false)
    private var serverSocket: SSLServerSocket? = null
    private val limiter = Semaphore(Config.MAX_CONCURRENT_CONNECTIONS)

    /** Bind and start accepting; returns the actual port (pass 0 for ephemeral). */
    fun start(port: Int): Int {
        check(running.compareAndSet(false, true)) { "receiver already running" }
        val context = TlsContexts.serverContext(identity)
        val socket = context.serverSocketFactory.createServerSocket(port) as SSLServerSocket
        socket.needClientAuth = true
        socket.enabledProtocols = arrayOf("TLSv1.3")
        serverSocket = socket
        Thread({ acceptLoop(socket) }, "lanecho-accept").apply { isDaemon = true }.start()
        return socket.localPort
    }

    fun stop() {
        running.set(false)
        runCatching { serverSocket?.close() }
        serverSocket = null
    }

    private fun acceptLoop(server: SSLServerSocket) {
        while (running.get()) {
            val connection = try {
                server.accept() as SSLSocket
            } catch (_: Exception) {
                if (!running.get()) return
                continue
            }
            if (!limiter.tryAcquire()) {
                Log.warn("inbound connection cap reached, refusing ${connection.inetAddress}")
                runCatching { connection.close() }
                continue
            }
            Thread({
                try {
                    serve(connection)
                } finally {
                    limiter.release()
                    runCatching { connection.close() }
                }
            }, "lanecho-conn").apply { isDaemon = true }.start()
        }
    }

    // One connection: the whole unauthenticated stage (TLS handshake + Hello)
    // under the handshake deadline, then the idle-bounded transaction loop
    private fun serve(socket: SSLSocket) {
        socket.tcpNoDelay = true
        socket.soTimeout = Config.HANDSHAKE_TIMEOUT_MS.toInt()
        val remote = try {
            socket.startHandshake()
            helloGate(socket)
        } catch (e: Exception) {
            Log.warn("inbound handshake failed: ${e.message}")
            return
        } ?: return

        socket.soTimeout = Config.IDLE_TIMEOUT_MS.toInt()
        try {
            transactionLoop(socket, remote)
        } catch (e: Exception) {
            Log.warn("inbound session ended abnormally: ${e.message}")
        }
        // Bye is the dialer's last frame, so closing here cannot RST unread
        // data; our close_notify is the EOF its drain is waiting for
    }

    // Hello gate: certificate fingerprint from the TLS layer, version major,
    // and the declared fingerprint must equal the certificate's. Unlike the
    // Rust side (silent drop) this answers with a reason first, matching the
    // Swift receiver — dialers surface it verbatim
    private fun helloGate(socket: SSLSocket): PeerInfo? {
        val certificateFingerprint = TlsContexts.peerFingerprint(socket.session)
        if (certificateFingerprint == null) {
            Log.warn("inbound connection carried no client certificate")
            return null
        }
        val first = readFrame(socket.inputStream)
        if (first !is ControlMessage.Hello) {
            Log.warn("first inbound frame was ${first.kind}, expected hello")
            return null
        }
        if (!versionCompatible(first.version)) {
            writeFrame(socket.outputStream, ControlMessage.SyncRejected(ReasonCode.UNSUPPORTED_VERSION))
            return null
        }
        if (first.info.fingerprint != certificateFingerprint) {
            writeFrame(socket.outputStream, ControlMessage.SyncRejected(ReasonCode.IDENTITY_MISMATCH))
            return null
        }
        writeFrame(socket.outputStream, ControlMessage.HelloAck(PROTOCOL_VERSION, localInfo()))
        return first.info
    }

    private fun transactionLoop(socket: SSLSocket, remote: PeerInfo) {
        while (true) {
            val message = try {
                readFrame(socket.inputStream)
            } catch (_: Exception) {
                // Idle timeout, dropped connection or a broken frame: leave
                return
            }
            when (message) {
                is ControlMessage.PairRequest -> {
                    val accepted = callbacks.decidePair(remote)
                    writeFrame(socket.outputStream, ControlMessage.PairResponse(accepted))
                }
                is ControlMessage.ClipboardSync -> when (message.contentType) {
                    ContentType.IMAGE -> serveImageBlob(socket, remote, message)
                    // v1 refuses file syncs outright (decision DA5)
                    ContentType.FILES ->
                        writeFrame(socket.outputStream, ControlMessage.SyncRejected(ReasonCode.UNSUPPORTED_TYPE))
                    else -> {
                        val verdict =
                            callbacks.acceptSync(remote, message.timestampMs, message.contentType, message.data)
                        val reply = if (verdict == null) ControlMessage.SyncAck
                        else ControlMessage.SyncRejected(verdict)
                        writeFrame(socket.outputStream, reply)
                    }
                }
                is ControlMessage.Unpair -> callbacks.onUnpair(remote)
                ControlMessage.Bye -> return
                else -> {
                    Log.warn("unexpected inbound message ${message.kind}, dropping connection")
                    return
                }
            }
        }
    }

    // Image blob reception: the whole check chain runs at the offer stage so a
    // doomed transfer is stopped before any bytes flow; the stream is counted
    // exactly against the declared total and verified against the footer hash
    private fun serveImageBlob(socket: SSLSocket, remote: PeerInfo, offer: ControlMessage.ClipboardSync) {
        // Unparsable metadata is a protocol violation (a 1.1 dialer never
        // sends bad meta): drop the connection
        val meta = wireJson.decodeFromString(ImageOfferMeta.serializer(), offer.data)
        when (val decision = callbacks.decideImageOffer(remote, offer.timestampMs, meta)) {
            is OfferDecision.Reject -> {
                writeFrame(socket.outputStream, ControlMessage.SyncRejected(decision.reasonCode))
                return
            }
            OfferDecision.Stale -> {
                writeFrame(socket.outputStream, ControlMessage.SyncAck)
                return
            }
            OfferDecision.Accept -> Unit
        }
        writeFrame(socket.outputStream, ControlMessage.BlobAccept)
        val (png, actualHash) = receiveExactly(socket.inputStream, meta.totalBytes)
        val footer = readFrame(socket.inputStream)
        if (footer !is ControlMessage.BlobFooter) {
            throw IllegalStateException("expected blob_footer, got ${footer.kind}")
        }
        val reply = if (footer.hash != actualHash) {
            Log.warn("image stream checksum mismatch from ${remote.name}, discarding")
            ControlMessage.SyncRejected(ReasonCode.CHECKSUM_MISMATCH)
        } else {
            when (val verdict = callbacks.finishImage(remote, offer.timestampMs, png)) {
                null -> ControlMessage.SyncAck
                else -> ControlMessage.SyncRejected(verdict)
            }
        }
        writeFrame(socket.outputStream, reply)
    }

    // Read exactly [total] raw bytes (the blob stream has no frame headers),
    // hashing while receiving; each read sits under the idle soTimeout
    private fun receiveExactly(input: InputStream, total: Long): Pair<ByteArray, String> {
        require(total in 1..Config.MAX_SYNC_IMAGE_BYTES) { "blob size $total out of range" }
        val out = ByteArray(total.toInt())
        val hasher = Blake3.newHasher()
        var offset = 0
        while (offset < out.size) {
            val read = input.read(out, offset, minOf(Config.SYNC_STREAM_BUF, out.size - offset))
            if (read == -1) throw IllegalStateException("blob stream ended early at $offset/$total")
            hasher.update(out, offset, offset + read)
            offset += read
        }
        return out to hasher.finalize().toHex()
    }
}
