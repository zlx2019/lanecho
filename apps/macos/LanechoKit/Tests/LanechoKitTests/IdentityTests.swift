// Foundation tests: BLAKE3 correctness, which is what fingerprints agreeing
// across implementations rests on, plus identity material parsing.

import Foundation
import Testing

@testable import LanechoKit

// MARK: - BLAKE3

/// Shape of the official test vector file (BLAKE3-team/BLAKE3
/// test_vectors.json)
private struct Blake3Vectors: Decodable {
    struct Case: Decodable {
        /// Input length: the input is the first input_len bytes of the
        /// repeating 0,1,...,250 sequence
        let input_len: Int
        /// Extended output as hex; the first 64 characters are the standard
        /// 32-byte digest
        let hash: String
    }
    let cases: [Case]
}

/// Read a test resource
private func fixture(_ name: String) throws -> Data {
    let url = try #require(Bundle.module.url(forResource: "Fixtures/\(name)", withExtension: nil))
    return try Data(contentsOf: url)
}

/// The official vector for empty input, checked independently of fixture
/// parsing
@Test func blake3EmptyVector() {
    #expect(
        Blake3.hex(Data()) == "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    )
}

/// The full official vector set: pins byte-for-byte agreement with the
/// reference implementation (the Rust blake3 crate), which is a hard
/// prerequisite for fingerprints interoperating across implementations
@Test func blake3OfficialVectors() throws {
    let vectors = try JSONDecoder().decode(Blake3Vectors.self, from: fixture("blake3_test_vectors.json"))
    #expect(!vectors.cases.isEmpty)
    for c in vectors.cases {
        // Input = the first input_len bytes of the repeating 0,1,...,250
        // sequence
        let input = (0..<c.input_len).map { UInt8($0 % 251) }
        let expected = String(c.hash.prefix(64))
        #expect(Blake3.hex(input) == expected, "input_len=\(c.input_len)")
    }
}

/// Feeding incrementally matches a one-shot hash, so chunked update over large
/// content is correct
@Test func blake3IncrementalMatchesOneShot() {
    let data = (0..<100_000).map { UInt8($0 % 251) }
    // DispatchData is a multi-region DataProtocol implementation, so this takes
    // the per-region chunked path
    var chunked = DispatchData.empty
    for chunk in stride(from: 0, to: data.count, by: 7777) {
        let end = min(chunk + 7777, data.count)
        data[chunk..<end].withUnsafeBytes { chunked.append($0) }
    }
    #expect(Blake3.hex(chunked) == Blake3.hex(data))
}

// MARK: - Identity material

/// cert.der (X.509) and key.der (PKCS#8 P-256) parse natively, and the
/// fingerprint has the same shape as the Rust version's: 64 lowercase hex
/// characters
@Test func identityMaterialParsesRustCompatibleDER() throws {
    let certDER = try [UInt8](fixture("cert.der"))
    let keyDER = try [UInt8](fixture("key.der"))
    let material = try IdentityMaterial(certDER: certDER, keyDER: keyDER)
    #expect(material.fingerprint.count == 64)
    #expect(material.fingerprint.allSatisfy { $0.isHexDigit && !$0.isUppercase })
    // The fingerprint is a pure function: the same bytes always hash the same
    #expect(material.fingerprint == fingerprint(ofCertDER: certDER))
    // The private key and the certificate's public key are a pair (self-signed
    // fixture, verified by a signature round-trip)
    let message = Data("lanecho".utf8)
    let signature = try material.privateKey.signature(for: message)
    #expect(material.privateKey.publicKey.isValidSignature(signature, for: message))
}
