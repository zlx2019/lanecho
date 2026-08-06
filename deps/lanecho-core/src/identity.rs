//! Device identity: who I am, and how I prove it.
//!
//! - On first launch, generates a persistent UUID plus a self-signed X.509
//!   certificate (rcgen), stored in the data directory
//! - The certificate's BLAKE3 fingerprint is the device's network identity —
//!   the MAC address is not used (modern systems randomize it by default,
//!   reading it needs extra permissions, and it is privacy-contentious)
//! - The display name defaults to the hostname; the LAN IP is for display
//!   only, and an IP change does not affect identity
//! - Trust model: mutual TLS 1.3 plus explicit pairing, see [`crate::tls`]

use std::fs;
use std::path::Path;

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Metadata file name (UUID, display name)
const META_FILE: &str = "identity.json";
/// File name of the DER-encoded self-signed certificate
const CERT_FILE: &str = "cert.der";
/// File name of the DER-encoded PKCS#8 private key
const KEY_FILE: &str = "key.der";

/// Identity layer errors
#[derive(Debug, Error)]
pub enum IdentityError {
    /// Reading or writing an identity file failed
    #[error("身份文件读写失败: {0}")]
    Io(#[from] std::io::Error),
    /// Certificate or key generation failed
    #[error("证书生成失败: {0}")]
    CertGen(#[from] rcgen::Error),
    /// Parsing the identity metadata (identity.json) failed
    #[error("身份元数据解析失败: {0}")]
    Meta(#[from] serde_json::Error),
}

/// Metadata persisted in identity.json
#[derive(Debug, Serialize, Deserialize)]
struct IdentityMeta {
    /// Unique device ID (UUID v4)
    device_id: String,
    /// User-chosen display name; None means follow the hostname
    display_name: Option<String>,
}

/// Device identity: unique ID plus TLS certificate material
#[derive(Debug)]
pub struct DeviceIdentity {
    /// Unique device ID (a UUID v4 generated on first launch)
    pub device_id: String,
    /// Name shown to others (user-editable, follows the hostname by default)
    pub display_name: String,
    /// BLAKE3 fingerprint of the certificate (lowercase hex), the device's
    /// identity on the network
    pub fingerprint: String,
    /// DER-encoded self-signed certificate (presented during the handshake)
    pub cert_der: CertificateDer<'static>,
    /// DER-encoded PKCS#8 private key (not exposed; take a copy via
    /// [`Self::key_der`])
    key_der: PrivateKeyDer<'static>,
}

impl DeviceIdentity {
    /// Load the identity from the data directory; if any of the three
    /// identity files is missing, generate a new identity and persist it
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        let complete = [META_FILE, CERT_FILE, KEY_FILE]
            .iter()
            .all(|f| dir.join(f).exists());
        if complete {
            Self::load(dir)
        } else {
            Self::create(dir)
        }
    }

    /// Load an existing identity from the data directory
    fn load(dir: &Path) -> Result<Self, IdentityError> {
        let cert_der = CertificateDer::from(fs::read(dir.join(CERT_FILE))?);
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fs::read(dir.join(KEY_FILE))?));
        // If the metadata is corrupt, rebuild it from the intact certificate:
        // the real device identity is the certificate fingerprint, and the
        // metadata is regenerable (fresh device_id, display name falls back).
        // Better than refusing to start on a parse failure with no way out
        let meta: IdentityMeta = match serde_json::from_slice(&fs::read(dir.join(META_FILE))?) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!("identity.json 解析失败, 以证书为准重建元数据: {e}");
                let meta = IdentityMeta {
                    device_id: uuid::Uuid::new_v4().to_string(),
                    display_name: None,
                };
                write_meta(dir, &meta)?;
                meta
            }
        };
        Ok(Self::from_parts(
            meta.device_id,
            meta.display_name.unwrap_or_else(default_display_name),
            cert_der,
            key_der,
        ))
    }

    /// Generate a new identity (UUID + self-signed certificate) and write it
    /// to the data directory
    fn create(dir: &Path) -> Result<Self, IdentityError> {
        let key_pair = rcgen::KeyPair::generate()?;
        let params = rcgen::CertificateParams::new(vec!["lanecho".to_string()])?;
        let cert = params.self_signed(&key_pair)?;
        let cert_der = cert.der().clone();
        let key_bytes = key_pair.serialize_der();
        let device_id = uuid::Uuid::new_v4().to_string();

        let meta = IdentityMeta {
            device_id: device_id.clone(),
            display_name: None,
        };
        fs::create_dir_all(dir)?;
        write_meta(dir, &meta)?;
        fs::write(dir.join(CERT_FILE), cert_der.as_ref())?;
        fs::write(dir.join(KEY_FILE), &key_bytes)?;
        tracing::info!(%device_id, "已生成新设备身份");

        Ok(Self::from_parts(
            device_id,
            default_display_name(),
            cert_der,
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes)),
        ))
    }

    /// Assemble an identity from existing material; the fingerprint is
    /// computed here, in one place
    fn from_parts(
        device_id: String,
        display_name: String,
        cert_der: CertificateDer<'static>,
        key_der: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            fingerprint: fingerprint_of(&cert_der),
            device_id,
            display_name,
            cert_der,
            key_der,
        }
    }

    /// Take a copy of the private key (building a rustls config needs owned
    /// material)
    pub fn key_der(&self) -> PrivateKeyDer<'static> {
        self.key_der.clone_key()
    }

    /// Build the device info exchanged during handshake and discovery
    pub fn peer_info(&self) -> crate::protocol::PeerInfo {
        crate::protocol::PeerInfo {
            device_id: self.device_id.clone(),
            name: self.display_name.clone(),
            fingerprint: self.fingerprint.clone(),
            platform: platform(),
            os_version: Some(os_version().to_string()),
        }
    }
}

/// Persist the display name to identity.json (None restores following the
/// hostname)
///
/// Only the metadata changes; the certificate and key are untouched, so the
/// fingerprint (the device identity) stays the same. Callers then call
/// [`DeviceIdentity::load_or_create`] again for a fresh snapshot.
pub fn persist_display_name(dir: &Path, name: Option<&str>) -> Result<(), IdentityError> {
    let mut meta: IdentityMeta = serde_json::from_slice(&fs::read(dir.join(META_FILE))?)?;
    meta.display_name = name.map(str::to_string);
    write_meta(dir, &meta)
}

/// Atomic write of the meta file (tmp + rename): an interrupted in-place
/// overwrite leaves half a JSON document behind, and the next launch takes the
/// rebuild path and burns a new device_id for nothing
fn write_meta(dir: &Path, meta: &IdentityMeta) -> Result<(), IdentityError> {
    let tmp = dir.join(format!("{META_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    fs::rename(&tmp, dir.join(META_FILE))?;
    Ok(())
}

/// Platform identifier for this machine (macos / windows / linux)
pub fn platform() -> String {
    std::env::consts::OS.to_string()
}

/// OS version description for this machine (e.g. "macOS 15.3.1")
///
/// Detection costs a syscall, so a OnceLock caches the result for the process
/// lifetime (peer_info is called often on the heartbeat and handshake paths).
pub fn os_version() -> &'static str {
    static OS_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OS_VERSION.get_or_init(|| {
        let info = os_info::get();
        // os_info names the macOS type "Mac OS"; use the official spelling
        let name = match info.os_type() {
            os_info::Type::Macos => "macOS".to_string(),
            t => t.to_string(),
        };
        format!("{} {}", name, info.version())
    })
}

/// Compute the certificate fingerprint: lowercase hex of BLAKE3(cert_der)
///
/// The fingerprint is only a device identifier internal to lanecho, with no
/// cross-tool interoperability requirement, so it uses BLAKE3 — already a
/// dependency and faster — rather than SHA-256, avoiding an extra crate.
pub fn fingerprint_of(cert: &CertificateDer<'_>) -> String {
    blake3::hash(cert.as_ref()).to_hex().to_string()
}

/// Default display name: the hostname, falling back to the fixed "lanecho"
fn default_display_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "lanecho".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dedicated temp directory, cleaned up on Drop
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lanecho-id-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Loading again after the first generation must yield the same identity
    /// (persistence works)
    #[test]
    fn create_then_load_is_stable() {
        let dir = TempDir::new();
        let a = DeviceIdentity::load_or_create(&dir.0).unwrap();
        let b = DeviceIdentity::load_or_create(&dir.0).unwrap();
        assert_eq!(a.device_id, b.device_id);
        assert_eq!(a.fingerprint, b.fingerprint);
    }

    /// The fingerprint must be BLAKE3 as 64 lowercase hex digits
    #[test]
    fn fingerprint_is_hex64() {
        let dir = TempDir::new();
        let id = DeviceIdentity::load_or_create(&dir.0).unwrap();
        assert_eq!(id.fingerprint.len(), 64);
        assert!(
            id.fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    /// Two identities generated in different directories must have different
    /// fingerprints
    #[test]
    fn identities_are_unique() {
        let (d1, d2) = (TempDir::new(), TempDir::new());
        let a = DeviceIdentity::load_or_create(&d1.0).unwrap();
        let b = DeviceIdentity::load_or_create(&d2.0).unwrap();
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_ne!(a.device_id, b.device_id);
    }

    /// OS version detection must produce a non-empty description, which is
    /// then broadcast with the identity
    #[test]
    fn os_version_is_detected() {
        let v = os_version();
        println!("detected os version: {v}");
        assert!(!v.trim().is_empty());
    }
}
