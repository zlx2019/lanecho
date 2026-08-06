// Parsing and verification of identity material (certificate + private key).
//
// The cert.der (X.509 DER) and key.der (PKCS#8 P-256 DER) written by the Rust
// build parse natively in Swift, which is what lets both clients share one
// data directory with an unchanged fingerprint.

import Crypto
import Foundation
import X509

/// Identity material: the parse result of `cert.der` / `key.der`, in the same
/// format as the Rust build
public struct IdentityMaterial: Sendable {
    /// Raw certificate DER bytes: the only input to both presenting the TLS
    /// certificate and computing the fingerprint
    public let certificateDER: [UInt8]
    /// The parsed certificate, for debugging and display; the trust model
    /// looks only at the fingerprint, never at the contents
    public let certificate: Certificate
    /// P-256 private key
    public let privateKey: P256.Signing.PrivateKey
    /// Certificate fingerprint (lowercase BLAKE3 hex); the network identity
    public let fingerprint: String

    /// Load from DER bytes; if either piece fails to parse it throws, and the
    /// caller takes the "generate a new identity" path
    public init(certDER: [UInt8], keyDER: [UInt8]) throws {
        self.certificateDER = certDER
        self.certificate = try Certificate(derEncoded: certDER)
        self.privateKey = try P256.Signing.PrivateKey(derRepresentation: Data(keyDER))
        self.fingerprint = LanechoKit.fingerprint(ofCertDER: certDER)
    }
}
