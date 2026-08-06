// BLAKE3 hashing: a Swift wrapper over the official C implementation.
//
// The single hash function behind both fingerprints and content hashes.
// Fingerprints must be byte-for-byte identical to the Rust build (the blake3
// crate); that is the hard constraint for cross-implementation interop.

import CBlake3
import Foundation

/// BLAKE3 hashing
public enum Blake3 {
    /// Digest length in bytes
    public static let outputByteCount = 32

    /// One-shot hash, returning the 32-byte digest
    public static func hash(_ data: some DataProtocol) -> [UInt8] {
        var hasher = blake3_hasher()
        blake3_hasher_init(&hasher)
        for region in data.regions {
            region.withUnsafeBytes { buf in
                blake3_hasher_update(&hasher, buf.baseAddress, buf.count)
            }
        }
        var out = [UInt8](repeating: 0, count: outputByteCount)
        blake3_hasher_finalize(&hasher, &out, outputByteCount)
        return out
    }

    /// One-shot hash, returning lowercase hex — the same shape as Rust's
    /// `to_hex()`, 64 characters
    public static func hex(_ data: some DataProtocol) -> String {
        hash(data).map { String(format: "%02x", $0) }.joined()
    }

    /// Streaming hasher, hashing a raw blob stream as it is transferred; same
    /// semantics as the Rust blake3::Hasher
    public final class Hasher {
        private var state = blake3_hasher()

        /// Initialize an empty hasher
        public init() {
            blake3_hasher_init(&state)
        }

        /// Feed in a run of bytes
        public func update(_ bytes: UnsafeRawBufferPointer) {
            blake3_hasher_update(&state, bytes.baseAddress, bytes.count)
        }

        /// Feed in a run of bytes (convenience entry point for arrays and Data)
        public func update(_ data: some DataProtocol) {
            for region in data.regions {
                region.withUnsafeBytes { update($0) }
            }
        }

        /// Finish into lowercase hex (64 characters); finalize does not
        /// consume the state, but treat it as one-shot
        public func finalizeHex() -> String {
            var out = [UInt8](repeating: 0, count: Blake3.outputByteCount)
            blake3_hasher_finalize(&state, &out, Blake3.outputByteCount)
            return out.map { String(format: "%02x", $0) }.joined()
        }
    }
}
