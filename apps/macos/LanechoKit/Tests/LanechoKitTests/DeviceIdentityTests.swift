// DeviceIdentity tests: mirrors the Rust identity.rs test surface and guards
// the on-disk file format both clients share.

import Crypto
import Foundation
import Testing

@testable import LanechoKit

/// An isolated temporary directory, removed once the body returns
private func withTempDir<T>(_ body: (URL) throws -> T) throws -> T {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("lanecho-id-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: dir) }
    return try body(dir)
}

/// Loading again after the first generation yields the same identity, so
/// persistence is correct
@Test func createThenLoadIsStable() throws {
    try withTempDir { dir in
        let a = try DeviceIdentity.loadOrCreate(dir: dir)
        let b = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(a.deviceId == b.deviceId)
        #expect(a.fingerprint == b.fingerprint)
    }
}

/// The fingerprint is a BLAKE3 digest as 64 lowercase hex characters
@Test func fingerprintIsHex64() throws {
    try withTempDir { dir in
        let id = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(id.fingerprint.count == 64)
        #expect(id.fingerprint.allSatisfy { $0.isHexDigit && !$0.isUppercase })
    }
}

/// Two identities generated in different directories must have different
/// fingerprints
@Test func identitiesAreUnique() throws {
    try withTempDir { d1 in
        try withTempDir { d2 in
            let a = try DeviceIdentity.loadOrCreate(dir: d1)
            let b = try DeviceIdentity.loadOrCreate(dir: d2)
            #expect(a.fingerprint != b.fingerprint)
            #expect(a.deviceId != b.deviceId)
        }
    }
}

/// A corrupt meta file is rebuilt from the certificate: the fingerprint is
/// unchanged, device_id is regenerated, and startup is not refused
@Test func corruptMetaRebuildsKeepingCertificate() throws {
    try withTempDir { dir in
        let original = try DeviceIdentity.loadOrCreate(dir: dir)
        try Data("{broken".utf8).write(to: dir.appendingPathComponent("identity.json"))
        let rebuilt = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(rebuilt.fingerprint == original.fingerprint)
        #expect(rebuilt.deviceId != original.deviceId)
        // The rebuilt meta is repaired on disk, so a further load is stable
        let again = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(again.deviceId == rebuilt.deviceId)
    }
}

/// Display name: persisting and clearing round-trips; the fingerprint, which is
/// the device identity, is untouched
@Test func displayNamePersistRoundtrip() throws {
    try withTempDir { dir in
        let original = try DeviceIdentity.loadOrCreate(dir: dir)
        try DeviceIdentity.persistDisplayName(dir: dir, name: "Workstation")
        let renamed = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(renamed.displayName == "Workstation")
        #expect(renamed.fingerprint == original.fingerprint)
        // Clearing falls back to the host name default
        try DeviceIdentity.persistDisplayName(dir: dir, name: nil)
        let cleared = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(cleared.displayName == original.displayName)
    }
}

/// File format guardrail: the three files written to disk have the same shape
/// as the Rust version's
@Test func filesMatchRustLayout() throws {
    try withTempDir { dir in
        let id = try DeviceIdentity.loadOrCreate(dir: dir)
        // identity.json: snake_case keys, display name explicitly null
        let meta = try #require(
            try JSONSerialization.jsonObject(
                with: Data(contentsOf: dir.appendingPathComponent("identity.json")))
                as? [String: Any])
        #expect(meta["device_id"] as? String == id.deviceId)
        #expect(meta.keys.contains("display_name"))
        // key.der: PKCS#8, read directly by CryptoKit (the same format rustls
        // uses on the Rust side)
        let keyDER = try Data(contentsOf: dir.appendingPathComponent("key.der"))
        #expect(throws: Never.self) {
            _ = try P256.Signing.PrivateKey(derRepresentation: keyDER)
        }
        // cert.der: reloading it on its own and recomputing the fingerprint
        // gives the same value
        let certDER = try [UInt8](Data(contentsOf: dir.appendingPathComponent("cert.der")))
        #expect(fingerprint(ofCertDER: certDER) == id.fingerprint)
        // peerInfo shape
        let info = id.peerInfo()
        #expect(info.platform == "macos")
        #expect(info.osVersion?.hasPrefix("macOS ") == true)
    }
}

/// A self-generated identity builds TLS contexts directly: NIOSSL accepts the
/// DER chain we produce
@Test func generatedIdentityBuildsTLSContexts() throws {
    try withTempDir { dir in
        let id = try DeviceIdentity.loadOrCreate(dir: dir)
        #expect(throws: Never.self) {
            _ = try TLSContexts(material: id.material)
        }
    }
}
