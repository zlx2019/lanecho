package io.github.zlx2019.lanecho.core.identity

import io.github.zlx2019.lanecho.core.blake3.Blake3
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.security.KeyFactory
import java.security.Signature
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.PKCS8EncodedKeySpec
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class DeviceIdentityTest {

    @TempDir
    lateinit var dir: Path

    @Test
    fun createPersistsThreeFilesAndReloadsIdentically() {
        val created = DeviceIdentity.loadOrCreate(dir)
        for (file in listOf(META_FILE, CERT_FILE, KEY_FILE)) {
            assertTrue(Files.exists(dir.resolve(file)), "$file missing")
        }
        val reloaded = DeviceIdentity.loadOrCreate(dir)
        assertEquals(created.deviceId, reloaded.deviceId)
        assertEquals(created.fingerprint, reloaded.fingerprint)
        assertNull(reloaded.displayName)
    }

    @Test
    fun fingerprintIsBlake3OfCertificateDer() {
        val identity = DeviceIdentity.loadOrCreate(dir)
        assertEquals(64, identity.fingerprint.length)
        assertTrue(identity.fingerprint.all { it in "0123456789abcdef" })
        assertEquals(Blake3.hashHex(identity.certificateDer), identity.fingerprint)
    }

    // identity.json corrupt + certificate intact = fresh device_id, same
    // fingerprint, metadata repaired on disk (Rust load() semantics)
    @Test
    fun corruptMetaRebuildsFreshDeviceIdKeepingFingerprint() {
        val original = DeviceIdentity.loadOrCreate(dir)
        Files.writeString(dir.resolve(META_FILE), "{ not json")
        val reloaded = DeviceIdentity.loadOrCreate(dir)
        assertEquals(original.fingerprint, reloaded.fingerprint)
        assertNotEquals(original.deviceId, reloaded.deviceId)
        // The rebuilt metadata must be parseable again
        val repaired = DeviceIdentity.loadOrCreate(dir)
        assertEquals(reloaded.deviceId, repaired.deviceId)
    }

    @Test
    fun missingAnyFileGeneratesFreshIdentity() {
        val original = DeviceIdentity.loadOrCreate(dir)
        Files.delete(dir.resolve(KEY_FILE))
        val fresh = DeviceIdentity.loadOrCreate(dir)
        assertNotEquals(original.fingerprint, fresh.fingerprint)
        assertTrue(Files.exists(dir.resolve(KEY_FILE)))
    }

    @Test
    fun renamePersistsAndKeepsCertificate() {
        val original = DeviceIdentity.loadOrCreate(dir)
        val renamed = original.withDisplayName(dir, "Zero's Phone")
        assertEquals("Zero's Phone", renamed.displayName)
        assertEquals(original.fingerprint, renamed.fingerprint)
        val reloaded = DeviceIdentity.loadOrCreate(dir)
        assertEquals("Zero's Phone", reloaded.displayName)
        assertEquals(original.deviceId, reloaded.deviceId)
    }

    // The JCA layer itself must accept what we persisted: parse the
    // certificate, restore the PKCS#8 key, and complete a sign/verify round
    // trip against the certificate's public key (transport-level sanity
    // before the TLS milestone)
    @Test
    fun jcaParsesCertAndKeySignatureRoundTrip() {
        val identity = DeviceIdentity.loadOrCreate(dir)
        val cert = CertificateFactory.getInstance("X.509")
            .generateCertificate(identity.certificateDer.inputStream()) as X509Certificate
        assertEquals("EC", cert.publicKey.algorithm)
        val key = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(identity.privateKeyDer))

        val payload = "lanecho".encodeToByteArray()
        val signer = Signature.getInstance("SHA256withECDSA")
        signer.initSign(key)
        signer.update(payload)
        val signature = signer.sign()
        val verifier = Signature.getInstance("SHA256withECDSA")
        verifier.initVerify(cert.publicKey)
        verifier.update(payload)
        assertTrue(verifier.verify(signature))
    }

    // Cross-implementation spike (Swift M0 R2 equivalent): openssl must parse
    // our DER output the same way it parses rcgen's. Skipped when openssl is
    // not on PATH (CI images and dev machines both have it in practice)
    @Test
    fun opensslParsesGeneratedCertAndKey() {
        assumeTrue(commandAvailable("openssl"), "openssl not available")
        val identity = DeviceIdentity.loadOrCreate(dir)

        val certText = runTool("openssl", "x509", "-inform", "der", "-noout", "-text", "-in", dir.resolve(CERT_FILE).toString())
        assertTrue("prime256v1" in certText || "P-256" in certText, "not P-256: $certText")
        assertTrue("lanecho" in certText, "SAN/CN missing: $certText")
        assertTrue("ecdsa-with-SHA256" in certText, "not SHA256withECDSA: $certText")

        val keyText = runTool("openssl", "pkey", "-inform", "der", "-noout", "-text", "-in", dir.resolve(KEY_FILE).toString())
        assertTrue("prime256v1" in keyText || "P-256" in keyText, "key not P-256: $keyText")
        // Suppress unused warning: identity is only needed for its side effect
        assertEquals(64, identity.fingerprint.length)
    }

    private fun commandAvailable(name: String): Boolean = try {
        ProcessBuilder("which", name).start().waitFor() == 0
    } catch (_: Exception) {
        false
    }

    private fun runTool(vararg command: String): String {
        val process = ProcessBuilder(*command).redirectErrorStream(true).start()
        val output = process.inputStream.readBytes().decodeToString()
        val code = process.waitFor()
        check(code == 0) { "${command.joinToString(" ")} exited $code: $output" }
        return output
    }
}
