//! Session layer: the frame protocol of the control channel.
//!
//! Frame format: a 4-byte big-endian length prefix + a JSON body.
//! Connections are "dial, transact, leave": every pairing or sync opens a new
//! TLS connection whose first frame must be `Hello`, and once the transaction
//! is done it is closed gracefully with `Bye`.
//!
//! Session transactions (from the initiator's view):
//! ```text
//! Hello → HelloAck → PairRequest   → PairResponse          → Bye   (pair)
//! Hello → HelloAck → ClipboardSync → SyncAck|SyncRejected  → Bye   (sync)
//! Hello → HelloAck → Unpair                                → Bye   (unpair)
//! ```

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::PROTOCOL_VERSION;

/// Maximum length of a single frame (1 MiB); guards against a malicious
/// oversized frame blowing up memory
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// Protocol layer error
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Underlying IO failure (including the peer disconnecting)
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// The frame length exceeds [`MAX_FRAME_LEN`]
    #[error("帧长度 {0} 字节超过上限 {MAX_FRAME_LEN}")]
    FrameTooLarge(u32),
    /// JSON encoding or decoding of the message failed
    #[error("消息编解码失败: {0}")]
    Codec(#[from] serde_json::Error),
    /// The protocol major versions differ, so communication is refused
    #[error("协议版本不兼容: 对端 {peer}, 本机 {local}")]
    VersionMismatch {
        /// Peer version
        peer: String,
        /// Local version
        local: String,
    },
    /// A message arrived that does not fit the current session state
    #[error("意外的消息: 期望 {expected}, 收到 {got}")]
    Unexpected {
        /// Expected message type
        expected: &'static str,
        /// Description of what actually arrived
        got: String,
    },
}

/// Device info, exchanged during the handshake (no avatar — lanecho has no
/// avatar mechanism)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Unique device ID (UUID)
    pub device_id: String,
    /// Display name
    pub name: String,
    /// BLAKE3 fingerprint of the certificate (hex)
    pub fingerprint: String,
    /// Platform tag (macos/windows/linux)
    pub platform: String,
    /// OS version description (e.g. "macOS 15.3.1"); optional, and absent in
    /// older versions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
}

/// Content type of a sync
///
/// Deliberately a String rather than an enum: when v2 adds types such as
/// images, an older version can still parse the whole frame and refuse it
/// explicitly with `SyncRejected(unsupported_type)` instead of dropping the
/// connection on a JSON parse failure.
pub mod content_type {
    /// Plain text
    pub const TEXT: &str = "text";
    /// Image (since 1.1; the offer's data is [`super::ImageOfferMeta`] JSON)
    pub const IMAGE: &str = "image";
    /// Files (since 1.1; the offer's data is [`super::FilesOfferMeta`] JSON)
    pub const FILES: &str = "files";
}

/// Structured rejection codes: the sender renders them in its own language,
/// and an unknown code renders as generic failure text. Also a String, to keep
/// the protocol open to evolution.
pub mod reason_code {
    /// The source is not paired
    pub const NOT_PAIRED: &str = "not_paired";
    /// The payload exceeds the receiver's limit
    pub const TOO_LARGE: &str = "too_large";
    /// The receiver has paused syncing
    pub const DISABLED: &str = "disabled";
    /// Unsupported content type (a 1.0 peer received a blob offer, or the
    /// type toggle is off)
    pub const UNSUPPORTED_TYPE: &str = "unsupported_type";
    /// Integrity check of the blob stream failed (since 1.1, and only in blob
    /// transactions)
    pub const CHECKSUM_MISMATCH: &str = "checksum_mismatch";
}

/// Metadata of an image offer (a JSON string carried in clipboard_sync.data;
/// 1.1)
///
/// To a 1.0 peer the offer is a **perfectly valid frame**: it answers an
/// unknown content_type with `sync_rejected(unsupported_type)` and the dialing
/// side skips that peer — compatibility does not rest on version sniffing but
/// on the protocol's own rejection path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOfferMeta {
    /// Total length of the raw byte stream that follows (PNG-encoded bytes)
    pub total_bytes: u64,
    /// Pixel width (for the pre-check and the notification text; what lands is
    /// whatever decoding yields)
    pub width: usize,
    /// Pixel height
    pub height: usize,
}

/// Metadata of a single file (1.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    /// File name (the name only, no path; the receiver sanitizes it anyway and
    /// does not trust the peer)
    pub name: String,
    /// Byte count of this file (how the concatenated stream is split up)
    pub bytes: u64,
}

/// Metadata of a file offer (a JSON string carried in clipboard_sync.data;
/// 1.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesOfferMeta {
    /// Total length of the raw byte stream that follows (= the sum of the file
    /// byte counts, which the receiver checks for consistency)
    pub total_bytes: u64,
    /// File list (concatenated back to back in the stream in this order)
    pub files: Vec<FileMeta>,
}

/// Control channel message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Session handshake (initiator → receiver)
    Hello {
        /// Protocol version (major.minor)
        version: String,
        /// Initiator's device info
        info: PeerInfo,
    },
    /// Handshake reply (the receiver's device info)
    HelloAck {
        /// Protocol version (major.minor)
        version: String,
        /// Receiver's device info
        info: PeerInfo,
    },
    /// Pairing request: identities were already exchanged in Hello, so this
    /// frame only states the intent
    PairRequest,
    /// Pairing reply: what the peer's user chose in the dialog
    PairResponse {
        /// Whether the pairing was accepted
        accepted: bool,
    },
    /// Unpair notification (best effort; missing it does not weaken security,
    /// since the check lives on the receiving side)
    Unpair,
    /// Clipboard sync payload
    ClipboardSync {
        /// Sequence number, monotonic within the sending device (for logs and
        /// troubleshooting)
        seq: u64,
        /// When the copy happened (Unix milliseconds); the LWW tiebreaker
        timestamp_ms: u64,
        /// Content type (see [`content_type`]; v1 is text only)
        content_type: String,
        /// The content itself: delivered byte for byte, never trimmed or
        /// escaped
        data: String,
    },
    /// The blob offer was taken, so the requester may start sending the raw
    /// byte stream (1.1)
    ///
    /// Transaction order: after the offer the accepting side answers with
    /// BlobAccept (it wants the data), SyncAck (LWW judged it stale, so no
    /// transfer is needed) or SyncRejected (refused); once the stream ends the
    /// requester sends [`ControlMessage::BlobFooter`], and the accepting side
    /// checks the hash before answering with the final verdict.
    BlobAccept,
    /// End marker of a raw blob stream, carrying the BLAKE3 integrity value of
    /// the whole stream (1.1)
    BlobFooter {
        /// BLAKE3 hex of the entire stream (computed while transferring)
        hash: String,
    },
    /// The sync was accepted and written to the clipboard
    SyncAck,
    /// The sync was refused
    SyncRejected {
        /// Structured rejection code (see [`reason_code`]), which the sender
        /// renders in its own language
        reason_code: String,
    },
    /// Close the session gracefully
    Bye,
}

impl ControlMessage {
    /// Short name of the message type (for logs and error messages)
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::HelloAck { .. } => "hello_ack",
            Self::PairRequest => "pair_request",
            Self::PairResponse { .. } => "pair_response",
            Self::Unpair => "unpair",
            Self::ClipboardSync { .. } => "clipboard_sync",
            Self::BlobAccept => "blob_accept",
            Self::BlobFooter { .. } => "blob_footer",
            Self::SyncAck => "sync_ack",
            Self::SyncRejected { .. } => "sync_rejected",
            Self::Bye => "bye",
        }
    }
}

/// Pre-encode one frame (4-byte big-endian length + JSON body) into contiguous
/// bytes
///
/// A broadcast sends **the same frame** to every target node, so it is encoded
/// once, shared as an `Arc<[u8]>` and written out through [`write_raw_frame`],
/// which avoids re-serializing the whole text per node.
pub fn encode_frame(msg: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge(u32::MAX))?;
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(len));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Write out one frame of bytes pre-encoded by [`encode_frame`], then flush
pub async fn write_raw_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &[u8],
) -> Result<(), ProtocolError> {
    w.write_all(frame).await?;
    w.flush().await?;
    Ok(())
}

/// Write a frame: encode it, then write the whole thing in one go (length
/// prefix and body are contiguous, saving a system call)
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    msg: &ControlMessage,
) -> Result<(), ProtocolError> {
    let frame = encode_frame(msg)?;
    write_raw_frame(w, &frame).await
}

/// Read one frame and decode it; an oversized frame errors out and
/// disconnects without its content being read
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<ControlMessage, ProtocolError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

/// Check the peer's protocol version: the same major counts as compatible
pub fn check_version(peer_version: &str) -> Result<(), ProtocolError> {
    if major_of(peer_version) == major_of(PROTOCOL_VERSION) {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch {
            peer: peer_version.to_string(),
            local: PROTOCOL_VERSION.to_string(),
        })
    }
}

/// Take the major segment of a version string
fn major_of(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build device info for tests
    fn peer_info() -> PeerInfo {
        PeerInfo {
            device_id: "d1".into(),
            name: "n1".into(),
            fingerprint: "f".repeat(64),
            platform: "macos".into(),
            os_version: Some("macOS 15.3".into()),
        }
    }

    /// Frame codec roundtrip: every kind of message must come back unchanged
    /// through a duplex pipe
    #[tokio::test]
    async fn frame_roundtrip() {
        let samples = vec![
            ControlMessage::Hello {
                version: PROTOCOL_VERSION.to_string(),
                info: peer_info(),
            },
            ControlMessage::PairRequest,
            ControlMessage::PairResponse { accepted: true },
            // Text must stay byte for byte identical: leading and trailing
            // whitespace and control characters are included on purpose
            ControlMessage::ClipboardSync {
                seq: 7,
                timestamp_ms: 1_752_000_000_000,
                content_type: content_type::TEXT.to_string(),
                data: "  你好\n\t emoji🚀 \0 尾巴  ".into(),
            },
            ControlMessage::SyncRejected {
                reason_code: reason_code::NOT_PAIRED.to_string(),
            },
            ControlMessage::Bye,
        ];
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        for msg in &samples {
            write_frame(&mut a, msg).await.unwrap();
            let got = read_frame(&mut b).await.unwrap();
            assert_eq!(
                serde_json::to_string(&got).unwrap(),
                serde_json::to_string(msg).unwrap()
            );
        }
    }

    /// Serializing a sync message must not alter its content field (the guard
    /// rail for byte-for-byte fidelity)
    #[test]
    fn sync_data_is_byte_exact() {
        let raw = "  space  \u{7f} 中文 ";
        let msg = ControlMessage::ClipboardSync {
            seq: 0,
            timestamp_ms: 0,
            content_type: content_type::TEXT.to_string(),
            data: raw.into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        match serde_json::from_str(&json).unwrap() {
            ControlMessage::ClipboardSync { data, .. } => assert_eq!(data, raw),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// An unknown content type must still parse as an ordinary string (the
    /// guard rail for v2 evolution: an older version receiving a new type has
    /// to take the SyncRejected path, not fail to parse and disconnect)
    #[test]
    fn unknown_content_type_still_parses() {
        let json = r#"{"type":"clipboard_sync","seq":1,"timestamp_ms":2,"content_type":"image/png","data":"x"}"#;
        let msg: ControlMessage = serde_json::from_str(json).unwrap();
        let ControlMessage::ClipboardSync { content_type, .. } = msg else {
            panic!("expected clipboard_sync");
        };
        assert_eq!(content_type, "image/png");
    }

    /// Device info from an older version (without the os_version field) must
    /// parse, and an unset optional field must not be serialized (the
    /// backward compatibility guard rail)
    #[test]
    fn peer_info_optional_fields_are_backward_compatible() {
        let legacy = r#"{"device_id":"d","name":"n","fingerprint":"f","platform":"macos"}"#;
        let info: PeerInfo = serde_json::from_str(legacy).unwrap();
        assert_eq!(info.os_version, None);
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("os_version"));
    }

    /// A pre-encoded frame is byte for byte what write_frame produces, and
    /// read_frame can restore it (the guard rail for reusing one encoding
    /// across a broadcast)
    #[tokio::test]
    async fn raw_frame_matches_write_frame() {
        let msg = ControlMessage::ClipboardSync {
            seq: 3,
            timestamp_ms: 42,
            content_type: content_type::TEXT.to_string(),
            data: "  原样字节\n\t🚀 ".into(),
        };
        let frame = encode_frame(&msg).unwrap();
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        write_raw_frame(&mut a, &frame).await.unwrap();
        let got = read_frame(&mut b).await.unwrap();
        assert_eq!(
            serde_json::to_string(&got).unwrap(),
            serde_json::to_string(&msg).unwrap()
        );
    }

    /// A message over the frame limit must be refused while encoding (the
    /// pre-check before a broadcast relies on this)
    #[test]
    fn encode_frame_rejects_oversized() {
        let msg = ControlMessage::ClipboardSync {
            seq: 0,
            timestamp_ms: 0,
            content_type: content_type::TEXT.to_string(),
            data: "x".repeat(MAX_FRAME_LEN as usize + 1),
        };
        assert!(matches!(
            encode_frame(&msg),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    /// The reading side must refuse an oversized frame before parsing the body
    #[tokio::test]
    async fn oversized_frame_rejected() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let bogus_len = (MAX_FRAME_LEN + 1).to_be_bytes();
        tokio::io::AsyncWriteExt::write_all(&mut a, &bogus_len)
            .await
            .unwrap();
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    /// The same major is compatible, a different one is refused
    #[test]
    fn version_compat() {
        assert!(check_version("1.0").is_ok());
        assert!(check_version("1.9").is_ok());
        assert!(matches!(
            check_version("2.0"),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }
}
