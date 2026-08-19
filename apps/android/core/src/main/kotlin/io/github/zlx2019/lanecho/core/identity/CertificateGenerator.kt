package io.github.zlx2019.lanecho.core.identity

import org.bouncycastle.asn1.x500.X500Name
import org.bouncycastle.asn1.x509.Extension
import org.bouncycastle.asn1.x509.GeneralName
import org.bouncycastle.asn1.x509.GeneralNames
import org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder
import java.math.BigInteger
import java.security.KeyPairGenerator
import java.security.SecureRandom
import java.security.spec.ECGenParameterSpec
import java.util.Date

/** DER bytes of a freshly generated identity: certificate + PKCS#8 private key. */
class GeneratedIdentity(val certificateDer: ByteArray, val privateKeyDer: ByteArray)

// Self-signed ECDSA P-256 + SHA-256, SAN=lanecho (PROTOCOL §2). Certificate
// contents are irrelevant to peers — nothing does CA/name/validity checks,
// only the BLAKE3 fingerprint of the DER matters. BouncyCastle is used purely
// as an X.509 builder; key generation and signing go through the platform JCA
// provider so no BC provider registration is needed (avoids clashing with
// Android's built-in crippled BC)
object CertificateGenerator {
    fun generate(): GeneratedIdentity {
        val keyPairGenerator = KeyPairGenerator.getInstance("EC")
        keyPairGenerator.initialize(ECGenParameterSpec("secp256r1"), SecureRandom())
        val keyPair = keyPairGenerator.generateKeyPair()

        val name = X500Name("CN=lanecho")
        val now = System.currentTimeMillis()
        val dayMs = 24L * 60 * 60 * 1000
        // Backdate a day to tolerate clock skew; expiry is irrelevant (unpinned)
        val notBefore = Date(now - dayMs)
        val notAfter = Date(now + 100L * 365 * dayMs)
        val serial = BigInteger(63, SecureRandom())

        val builder = JcaX509v3CertificateBuilder(name, serial, notBefore, notAfter, name, keyPair.public)
        builder.addExtension(
            Extension.subjectAlternativeName,
            false,
            GeneralNames(GeneralName(GeneralName.dNSName, "lanecho")),
        )
        val signer = JcaContentSignerBuilder("SHA256withECDSA").build(keyPair.private)
        val certificateDer = builder.build(signer).encoded
        // JCA EC private keys encode as PKCS#8, matching rcgen's key.der
        return GeneratedIdentity(certificateDer, keyPair.private.encoded)
    }
}
