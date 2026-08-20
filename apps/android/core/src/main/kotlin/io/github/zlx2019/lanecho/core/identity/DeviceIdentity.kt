package io.github.zlx2019.lanecho.core.identity

import io.github.zlx2019.lanecho.core.blake3.Blake3
import io.github.zlx2019.lanecho.core.util.Log
import io.github.zlx2019.lanecho.core.util.atomicWrite
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.nio.file.Files
import java.nio.file.Path
import java.util.UUID

const val META_FILE = "identity.json"
const val CERT_FILE = "cert.der"
const val KEY_FILE = "key.der"

// identity.json shape shared with the desktop implementations (pretty JSON,
// snake_case, display_name null = follow the platform device name)
@Serializable
data class IdentityMeta(
    @SerialName("device_id") val deviceId: String,
    @SerialName("display_name") val displayName: String? = null,
)

private val metaJson = Json {
    prettyPrint = true
    prettyPrintIndent = "  "
    ignoreUnknownKeys = true
}

/**
 * The device identity: a self-signed certificate whose BLAKE3(DER) hex
 * fingerprint is the device's unique network identity (pairing/pinning key).
 */
class DeviceIdentity private constructor(
    val meta: IdentityMeta,
    val certificateDer: ByteArray,
    val privateKeyDer: ByteArray,
) {
    val deviceId: String get() = meta.deviceId
    val displayName: String? get() = meta.displayName

    /** 64 lowercase hex chars of BLAKE3 over the certificate DER (PROTOCOL §2). */
    val fingerprint: String = Blake3.hashHex(certificateDer)

    companion object {
        // Load the identity from the data directory; if any of the three
        // identity files is missing, generate a new identity and persist it
        // (same semantics as the Rust load_or_create)
        fun loadOrCreate(dir: Path): DeviceIdentity {
            val complete = listOf(META_FILE, CERT_FILE, KEY_FILE).all { Files.exists(dir.resolve(it)) }
            return if (complete) load(dir) else create(dir)
        }

        private fun load(dir: Path): DeviceIdentity {
            val certificateDer = Files.readAllBytes(dir.resolve(CERT_FILE))
            val privateKeyDer = Files.readAllBytes(dir.resolve(KEY_FILE))
            // If the metadata is corrupt, rebuild it from the intact
            // certificate: the real identity is the fingerprint, the metadata
            // is regenerable (fresh device_id, display name falls back)
            val meta = try {
                metaJson.decodeFromString<IdentityMeta>(Files.readAllBytes(dir.resolve(META_FILE)).decodeToString())
            } catch (e: Exception) {
                Log.warn("identity.json unreadable, rebuilding metadata from certificate: ${e.message}")
                val rebuilt = IdentityMeta(deviceId = UUID.randomUUID().toString(), displayName = null)
                writeMeta(dir, rebuilt)
                rebuilt
            }
            return DeviceIdentity(meta, certificateDer, privateKeyDer)
        }

        private fun create(dir: Path): DeviceIdentity {
            val generated = CertificateGenerator.generate()
            val meta = IdentityMeta(deviceId = UUID.randomUUID().toString(), displayName = null)
            atomicWrite(dir.resolve(CERT_FILE), generated.certificateDer)
            atomicWrite(dir.resolve(KEY_FILE), generated.privateKeyDer)
            writeMeta(dir, meta)
            return DeviceIdentity(meta, generated.certificateDer, generated.privateKeyDer)
        }

        private fun writeMeta(dir: Path, meta: IdentityMeta) {
            atomicWrite(dir.resolve(META_FILE), metaJson.encodeToString(IdentityMeta.serializer(), meta).encodeToByteArray())
        }
    }

    /** Persist a new display name (hot rename keeps certificate and fingerprint). */
    fun withDisplayName(dir: Path, name: String?): DeviceIdentity {
        val updated = meta.copy(displayName = name?.takeIf { it.isNotBlank() })
        atomicWrite(
            dir.resolve(META_FILE),
            metaJson.encodeToString(IdentityMeta.serializer(), updated).encodeToByteArray(),
        )
        return DeviceIdentity(updated, certificateDer, privateKeyDer)
    }
}
