//! TLS layer: mutual TLS 1.3 with self-signed certificates and fingerprint
//! verification.
//!
//! No CA chain is involved:
//! - The client verifies the server certificate by fingerprint pinning (the
//!   expected fingerprint comes from discovery or from user confirmation)
//! - The server requires a client certificate but performs no CA validation;
//!   after the handshake, the layer above compares fingerprints to enforce
//!   pairing
//! - The CLI's direct-IP integration scenario may accept any certificate, but
//!   the layer above must show the peer fingerprint to the user

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use thiserror::Error;

use crate::identity::{DeviceIdentity, fingerprint_of};

/// TLS layer errors
#[derive(Debug, Error)]
pub enum TlsError {
    /// Building the rustls config failed (invalid certificate or key, etc.)
    #[error("TLS 配置构建失败: {0}")]
    Config(#[from] rustls::Error),
}

/// Build the server TLS config: present this device's certificate and require
/// one from the client (no CA validation; pairing is checked by the layer above)
pub fn server_config(identity: &DeviceIdentity) -> Result<ServerConfig, TlsError> {
    let config = ServerConfig::builder()
        .with_client_cert_verifier(Arc::new(AcceptAnyClientCert::new()))
        .with_single_cert(vec![identity.cert_der.clone()], identity.key_der())?;
    Ok(config)
}

/// Build the client TLS config, presenting this device's certificate to
/// complete mutual authentication
///
/// A Some `expected_fingerprint` strictly pins the peer certificate's
/// fingerprint; None accepts any certificate — reserved for the CLI's direct
/// connection scenario, where the layer above must show the actual
/// fingerprint for the user to check
pub fn client_config(
    identity: &DeviceIdentity,
    expected_fingerprint: Option<String>,
) -> Result<ClientConfig, TlsError> {
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServerCert::new(expected_fingerprint)))
        .with_client_auth_cert(vec![identity.cert_der.clone()], identity.key_der())?;
    Ok(config)
}

/// Compute the fingerprint from a TLS connection's peer certificate chain
/// (uses the end-entity certificate)
pub fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Option<String> {
    certs.and_then(|c| c.first()).map(fingerprint_of)
}

/// Verify a TLS 1.2 handshake signature with the given provider (shared by
/// both verifiers)
fn verify_sig_tls12(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls12_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

/// Verify a TLS 1.3 handshake signature with the given provider (shared by
/// both verifiers)
fn verify_sig_tls13(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

/// Signature schemes the provider supports (shared by both verifiers)
fn supported_schemes(provider: &CryptoProvider) -> Vec<SignatureScheme> {
    provider
        .signature_verification_algorithms
        .supported_schemes()
}

/// Process-wide shared crypto provider
///
/// Building the algorithm table has a fixed cost, and every sync or pairing
/// transaction opens a new connection and assembles a verifier and rustls
/// config; sharing one instance avoids rebuilding it each time.
fn shared_provider() -> Arc<CryptoProvider> {
    static PROVIDER: std::sync::OnceLock<Arc<CryptoProvider>> = std::sync::OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider())))
}

/// Client-side verifier: pins the server certificate by fingerprint
#[derive(Debug)]
struct PinnedServerCert {
    /// Expected peer certificate fingerprint; None accepts any certificate
    expected: Option<String>,
    /// Crypto provider (used for TLS signature verification)
    provider: Arc<CryptoProvider>,
}

impl PinnedServerCert {
    /// Create the verifier, reusing the process-wide shared provider
    fn new(expected: Option<String>) -> Self {
        Self {
            expected,
            provider: shared_provider(),
        }
    }
}

impl ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match &self.expected {
            Some(expected) => {
                let actual = fingerprint_of(end_entity);
                if &actual == expected {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::General(format!(
                        "对端证书指纹不匹配: {actual}"
                    )))
                }
            }
            // No fingerprint to check; the layer above shows it to the user
            None => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls12(&self.provider, message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_schemes(&self.provider)
    }
}

/// Server-side verifier: accepts any client certificate and leaves the
/// pairing decision on the fingerprint to the layer above
#[derive(Debug)]
struct AcceptAnyClientCert {
    /// Crypto provider (used for TLS signature verification)
    provider: Arc<CryptoProvider>,
}

impl AcceptAnyClientCert {
    /// Create the verifier, reusing the process-wide shared provider
    fn new() -> Self {
        Self {
            provider: shared_provider(),
        }
    }
}

impl ClientCertVerifier for AcceptAnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        // Certificate validity is not checked here; the layer above decides
        // trust from the fingerprint and the paired set
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls12(&self.provider, message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_schemes(&self.provider)
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use super::*;

    /// A dedicated temp directory, cleaned up on Drop
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lanecho-tls-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Loopback over localhost: pinning the correct fingerprint completes the
    /// handshake, and both sides can read the other's certificate fingerprint
    #[tokio::test]
    async fn handshake_with_pinned_fingerprint() {
        let (d1, d2) = (TempDir::new(), TempDir::new());
        let server_id = Arc::new(DeviceIdentity::load_or_create(&d1.0).unwrap());
        let client_id = Arc::new(DeviceIdentity::load_or_create(&d2.0).unwrap());

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config(&server_id).unwrap()));

        let client_fp = client_id.fingerprint.clone();
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            // The server must obtain the client certificate and derive a
            // fingerprint matching that identity (the basis for TOFU)
            let fp = peer_fingerprint(tls.get_ref().1.peer_certificates()).unwrap();
            assert_eq!(fp, client_fp);
            let mut buf = [0u8; 4];
            tls.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping");
        });

        let connector = TlsConnector::from(Arc::new(
            client_config(&client_id, Some(server_id.fingerprint.clone())).unwrap(),
        ));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("lanecho").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        tls.write_all(b"ping").await.unwrap();
        tls.flush().await.unwrap();
        server_task.await.unwrap();
    }

    /// Pinning a wrong fingerprint must make the client handshake fail
    #[tokio::test]
    async fn handshake_rejects_wrong_fingerprint() {
        let (d1, d2) = (TempDir::new(), TempDir::new());
        let server_id = Arc::new(DeviceIdentity::load_or_create(&d1.0).unwrap());
        let client_id = Arc::new(DeviceIdentity::load_or_create(&d2.0).unwrap());

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config(&server_id).unwrap()));
        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                // The handshake is expected to fail; ignore the result
                let _ = acceptor.accept(tcp).await;
            }
        });

        let wrong_fp = "0".repeat(64);
        let connector =
            TlsConnector::from(Arc::new(client_config(&client_id, Some(wrong_fp)).unwrap()));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("lanecho").unwrap();
        assert!(connector.connect(name, tcp).await.is_err());
    }
}
