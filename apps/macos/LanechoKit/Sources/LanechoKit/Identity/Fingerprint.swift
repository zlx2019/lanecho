// Certificate fingerprint: a device's unique identity on the network

import Foundation

/// Compute the certificate fingerprint: BLAKE3 over the certificate DER bytes,
/// as lowercase hex (64 characters) — byte-for-byte identical to
/// `identity::fingerprint_of` on the Rust side
public func fingerprint(ofCertDER der: some DataProtocol) -> String {
    Blake3.hex(der)
}
