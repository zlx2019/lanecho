package io.github.zlx2019.lanecho.core.protocol

import io.github.zlx2019.lanecho.core.PROTOCOL_VERSION
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

// Control channel messages. The JSON shapes mirror the Rust
// `#[serde(tag = "type", rename_all = "snake_case")]` enum field for field
// (docs/PROTOCOL.md §4); golden tests pin them against captured Rust output.

/** Device info exchanged during handshake and discovery. */
@Serializable
data class PeerInfo(
    @SerialName("device_id") val deviceId: String,
    val name: String,
    /** BLAKE3 fingerprint of the certificate DER (64 lowercase hex chars). */
    val fingerprint: String,
    /** Platform tag; a free string so new platforms never break old peers. */
    val platform: String,
    /** Optional OS version; absent in older peers and never serialized as null. */
    @SerialName("os_version") val osVersion: String? = null,
)

// Deliberately strings rather than enums: an unknown content type must parse
// as a whole frame so the receiver can refuse it explicitly with
// sync_rejected(unsupported_type) instead of dropping the connection
object ContentType {
    const val TEXT = "text"
    const val IMAGE = "image"
    const val FILES = "files"
}

object ReasonCode {
    const val NOT_PAIRED = "not_paired"
    const val TOO_LARGE = "too_large"
    const val DISABLED = "disabled"
    const val UNSUPPORTED_TYPE = "unsupported_type"
    const val CHECKSUM_MISMATCH = "checksum_mismatch"
    const val IDENTITY_MISMATCH = "identity_mismatch"
    const val UNSUPPORTED_VERSION = "unsupported_version"
}

/** Image offer metadata (JSON string carried in clipboard_sync.data; 1.1). */
@Serializable
data class ImageOfferMeta(
    @SerialName("total_bytes") val totalBytes: Long,
    val width: Int,
    val height: Int,
)

/** One file's metadata within a files offer (1.1). */
@Serializable
data class FileMeta(
    /** Name only, never a path; the receiver sanitizes it regardless. */
    val name: String,
    val bytes: Long,
)

/** Files offer metadata (JSON string carried in clipboard_sync.data; 1.1). */
@Serializable
data class FilesOfferMeta(
    @SerialName("total_bytes") val totalBytes: Long,
    val files: List<FileMeta>,
)

@Serializable
sealed interface ControlMessage {
    @Serializable
    @SerialName("hello")
    data class Hello(val version: String, val info: PeerInfo) : ControlMessage

    @Serializable
    @SerialName("hello_ack")
    data class HelloAck(val version: String, val info: PeerInfo) : ControlMessage

    @Serializable
    @SerialName("pair_request")
    data object PairRequest : ControlMessage

    @Serializable
    @SerialName("pair_response")
    data class PairResponse(val accepted: Boolean) : ControlMessage

    @Serializable
    @SerialName("unpair")
    data object Unpair : ControlMessage

    @Serializable
    @SerialName("clipboard_sync")
    data class ClipboardSync(
        val seq: Long,
        /** Copy moment in Unix milliseconds; the LWW tiebreaker. */
        @SerialName("timestamp_ms") val timestampMs: Long,
        @SerialName("content_type") val contentType: String,
        /** Delivered byte for byte; never trimmed or escaped. */
        val data: String,
    ) : ControlMessage

    @Serializable
    @SerialName("blob_accept")
    data object BlobAccept : ControlMessage

    @Serializable
    @SerialName("blob_footer")
    data class BlobFooter(val hash: String) : ControlMessage

    @Serializable
    @SerialName("sync_ack")
    data object SyncAck : ControlMessage

    @Serializable
    @SerialName("sync_rejected")
    data class SyncRejected(@SerialName("reason_code") val reasonCode: String) : ControlMessage

    @Serializable
    @SerialName("bye")
    data object Bye : ControlMessage

    /** Short type name for logs and error messages. */
    val kind: String
        get() = when (this) {
            is Hello -> "hello"
            is HelloAck -> "hello_ack"
            PairRequest -> "pair_request"
            is PairResponse -> "pair_response"
            Unpair -> "unpair"
            is ClipboardSync -> "clipboard_sync"
            BlobAccept -> "blob_accept"
            is BlobFooter -> "blob_footer"
            SyncAck -> "sync_ack"
            is SyncRejected -> "sync_rejected"
            Bye -> "bye"
        }
}

// One Json instance for everything that crosses the wire: unknown fields are
// ignored (serde evolution rule) and null optionals are omitted, matching
// skip_serializing_if = "Option::is_none"
val wireJson: Json = Json {
    classDiscriminator = "type"
    ignoreUnknownKeys = true
    explicitNulls = false
}

/** Same protocol major = compatible (PROTOCOL.md §10). */
fun versionCompatible(peerVersion: String): Boolean =
    peerVersion.substringBefore('.') == PROTOCOL_VERSION.substringBefore('.')
