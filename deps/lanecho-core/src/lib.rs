//! lanecho-core: the LAN clipboard sync engine.
//!
//! A pure library, fully decoupled from any UI, shared by the CLI (used for
//! integration testing) and the Tauri desktop app. Layering:
//!
//! ```text
//! ┌─ discovery ─ peer discovery: mDNS as main channel, UDP multicast as fallback
//! ├─ identity  ─ device identity: UUID + BLAKE3 fingerprint of a self-signed cert
//! ├─ tls       ─ mutual TLS 1.3: fingerprint pinning, no CA chain
//! ├─ protocol  ─ control protocol: length-prefixed JSON frames, own frame set
//! └─ sync      ─ sync engine: clipboard polling / echo suppression / last-write-wins
//! ```
//!
pub mod clipboard;
pub mod config;
pub mod discovery;
pub mod identity;
pub mod protocol;
pub mod sync;
pub mod tls;

/// Protocol version (major.minor), negotiated during the handshake; a
/// differing major rejects the connection.
/// 1.1: image/file sync — a blob offer reuses the clipboard_sync frame
/// (content_type: image/files) and adds the blob_accept / blob_footer
/// messages; a 1.0 peer answers an offer with unsupported_type, so interop
/// still holds (minor records capabilities).
pub const PROTOCOL_VERSION: &str = "1.1";

/// Default TCP listen port (control channel); configurable in settings.
pub const DEFAULT_TCP_PORT: u16 = 42524;

/// Default UDP multicast discovery port (the fallback channel next to mDNS);
/// configurable in settings
pub const DEFAULT_DISCOVERY_PORT: u16 = 42525;

#[cfg(test)]
mod tests {
    use super::*;

    /// The protocol version must be a two-part major.minor string so the
    /// handshake negotiation can parse it
    #[test]
    fn protocol_version_format() {
        let parts: Vec<&str> = PROTOCOL_VERSION.split('.').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
    }

    /// The control port and the discovery port must not collide
    #[test]
    fn default_ports_distinct() {
        assert_ne!(DEFAULT_TCP_PORT, DEFAULT_DISCOVERY_PORT);
    }
}
