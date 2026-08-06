// TLS context assembly: TLS 1.3 mutual authentication + fingerprint trust
// model
//
// One-to-one with tls.rs on the Rust side (NIOSSL and rustls sit at the same
// level of abstraction):
// - Server (≈ AcceptAnyClientCert): presents the local certificate and
//   requires one from the client, but accepts any certificate — trust is not
//   decided in the TLS layer; after the handshake the layer above decides from
//   the fingerprint plus the pairing set
// - Client (≈ PinnedServerCert): presents the local certificate; a nil
//   expected fingerprint accepts any certificate (liveness probes and
//   bring-up, where the liveness decision is an explicit comparison made after
//   the handshake)
// Certificate and private key go straight from DER bytes on disk into memory;
// no keychain involved.

import Foundation
import NIOSSL

/// Mutually authenticated TLS context pair (built once per identity, reused
/// across connections)
public struct TLSContexts: Sendable {
    /// Server context, shared by the inbound listener
    public let server: NIOSSLContext
    /// Client context, shared by outbound dials; the fingerprint check happens
    /// in the handshake callback, so there is no need for a per-peer context
    public let client: NIOSSLContext

    /// Build from identity material
    public init(material: IdentityMaterial) throws {
        let cert = try NIOSSLCertificate(bytes: material.certificateDER, format: .der)
        // CryptoKit re-encodes the same PKCS#8 DER that key.der holds on disk
        let keyDER = [UInt8](material.privateKey.derRepresentation)
        let key = try NIOSSLPrivateKey(bytes: keyDER, format: .der)

        var serverConfig = TLSConfiguration.makeServerConfiguration(
            certificateChain: [.certificate(cert)],
            privateKey: .privateKey(key))
        serverConfig.minimumTLSVersion = .tlsv13
        // Require a client certificate (the server half of mutual auth);
        // whether that certificate is accepted is left to the custom
        // verification callback on the connection handler
        // (acceptAnyCertificate)
        serverConfig.certificateVerification = .noHostnameVerification
        self.server = try NIOSSLContext(configuration: serverConfig)

        var clientConfig = TLSConfiguration.makeClientConfiguration()
        clientConfig.minimumTLSVersion = .tlsv13
        clientConfig.certificateChain = [.certificate(cert)]
        clientConfig.privateKey = .privateKey(key)
        // No CA or hostname verification: trust is a pinned fingerprint (the
        // verification callback on the connection handler)
        clientConfig.certificateVerification = .noHostnameVerification
        self.client = try NIOSSLContext(configuration: clientConfig)
    }
}

/// Verification callback factory: accept any peer certificate and report the
/// end-entity fingerprint back to the caller (the server half — the pairing
/// verdict comes after the Hello gate)
public func acceptAnyCertificate(
    onFingerprint: @escaping @Sendable (String) -> Void
) -> NIOSSLCustomVerificationCallback {
    { certs, promise in
        guard let leaf = certs.first, let der = try? leaf.toDERBytes() else {
            promise.succeed(.failed)
            return
        }
        onFingerprint(fingerprint(ofCertDER: der))
        promise.succeed(.certificateVerified)
    }
}

/// Verification callback factory: pin the peer certificate to the expected
/// fingerprint (the client half); a nil `expected` accepts any certificate and
/// reports the actual fingerprint through the callback
public func pinnedCertificate(
    expected: String?,
    onFingerprint: (@Sendable (String) -> Void)? = nil
) -> NIOSSLCustomVerificationCallback {
    { certs, promise in
        guard let leaf = certs.first, let der = try? leaf.toDERBytes() else {
            promise.succeed(.failed)
            return
        }
        let actual = fingerprint(ofCertDER: der)
        onFingerprint?(actual)
        switch expected {
        case .some(let expected):
            promise.succeed(actual == expected ? .certificateVerified : .failed)
        case .none:
            promise.succeed(.certificateVerified)
        }
    }
}
