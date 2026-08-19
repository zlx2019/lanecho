package io.github.zlx2019.lanecho.core.transport

import io.github.zlx2019.lanecho.core.blake3.Blake3
import io.github.zlx2019.lanecho.core.identity.DeviceIdentity
import java.security.KeyFactory
import java.security.KeyStore
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.PKCS8EncodedKeySpec
import javax.net.ssl.KeyManager
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSession
import javax.net.ssl.SSLSocket
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

// TLS 1.3 mutual auth with fingerprint pinning (PROTOCOL.md §3). No CA, no
// hostname, no validity checks anywhere — identity is BLAKE3(cert DER) only.
// JSSE takes the DER identity through an in-memory KeyStore, so unlike the
// Swift implementation there is no keychain in the way
object TlsContexts {

    /** Receiver side: presents our certificate, demands a client certificate, accepts any. */
    fun serverContext(identity: DeviceIdentity): SSLContext =
        build(keyManagers(identity), PinningTrustManager(expectedFingerprint = null))

    /**
     * Dialer side: presents our certificate and pins the peer to [pinnedFingerprint].
     * Pass null for liveness probes, which compare explicitly after the handshake.
     */
    fun clientContext(identity: DeviceIdentity, pinnedFingerprint: String?): SSLContext =
        build(keyManagers(identity), PinningTrustManager(pinnedFingerprint))

    /** BLAKE3 hex of the peer's end-entity certificate, or null when absent. */
    fun peerFingerprint(session: SSLSession): String? = try {
        val chain = session.peerCertificates
        if (chain.isEmpty()) null else Blake3.hashHex(chain[0].encoded)
    } catch (_: Exception) {
        null
    }

    /** Restrict a socket to TLS 1.3 (the protocol floor; Rust dropped 1.2). */
    fun restrict(socket: SSLSocket) {
        socket.enabledProtocols = arrayOf("TLSv1.3")
    }

    private fun build(keyManagers: Array<KeyManager>, trustManager: X509TrustManager): SSLContext {
        val context = SSLContext.getInstance("TLS")
        context.init(keyManagers, arrayOf<TrustManager>(trustManager), SecureRandom())
        return context
    }

    private fun keyManagers(identity: DeviceIdentity): Array<KeyManager> {
        val certificate = CertificateFactory.getInstance("X.509")
            .generateCertificate(identity.certificateDer.inputStream()) as X509Certificate
        val privateKey = KeyFactory.getInstance("EC")
            .generatePrivate(PKCS8EncodedKeySpec(identity.privateKeyDer))
        val keyStore = KeyStore.getInstance("PKCS12")
        keyStore.load(null, null)
        keyStore.setKeyEntry("identity", privateKey, CharArray(0), arrayOf(certificate))
        val factory = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm())
        factory.init(keyStore, CharArray(0))
        return factory.keyManagers
    }

    // Client side pins the server certificate to one fingerprint; server side
    // accepts any client certificate (the pairing verdict happens after the
    // handshake against the paired set). Null expectation = accept and compare
    // later (probes)
    private class PinningTrustManager(private val expectedFingerprint: String?) : X509TrustManager {
        override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {
            if (chain.isEmpty()) throw CertificateException("client presented no certificate")
        }

        override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
            if (chain.isEmpty()) throw CertificateException("server presented no certificate")
            val expected = expectedFingerprint ?: return
            val actual = Blake3.hashHex(chain[0].encoded)
            if (actual != expected) {
                throw CertificateException("fingerprint mismatch: expected $expected, got $actual")
            }
        }

        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }
}
