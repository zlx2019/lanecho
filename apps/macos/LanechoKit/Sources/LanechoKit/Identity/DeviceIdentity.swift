// Device identity: who this device is and how it proves it. Mirrors the Rust
// identity.rs.
//
// - First launch generates a persistent UUID and a self-signed X.509
//   certificate, both stored in the data directory
// - The certificate's BLAKE3 fingerprint is the device's unique network
//   identity
// - The files have the **same names and formats** as the Rust build, so the
//   data directory is shared: identity.json (snake_case metadata) /
//   cert.der (X.509) / key.der (PKCS#8)
// - Corrupt metadata is rebuilt from the intact certificate (a fresh
//   device_id, the display name falls back), which beats refusing to start

import Crypto
import Foundation
import SwiftASN1
import X509

/// Identity layer error
public enum IdentityError: Error {
    /// Reading or writing an identity file failed
    case io(underlying: Error)
    /// Certificate generation failed
    case certGen(underlying: Error)
}

/// Metadata file name (UUID, display name)
private let metaFile = "identity.json"
/// File name of the DER-encoded self-signed certificate
private let certFile = "cert.der"
/// File name of the DER-encoded PKCS#8 private key
private let keyFile = "key.der"

/// Persisted shape of identity.json (snake_case, matching the Rust
/// IdentityMeta)
private struct IdentityMeta: Codable {
    /// Unique device ID (UUID v4)
    var deviceId: String
    /// User-set display name; nil means follow the hostname. Written as an
    /// explicit null, matching serde's output
    var displayName: String?

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case displayName = "display_name"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(deviceId, forKey: .deviceId)
        // An explicit null rather than omission: to_vec_pretty on the Rust
        // side writes null for an Option
        try c.encode(displayName, forKey: .displayName)
    }
}

/// Device identity: the unique identifier plus the TLS certificate material
public struct DeviceIdentity: Sendable {
    /// Unique device ID (a UUID v4 generated on first launch)
    public let deviceId: String
    /// Display name shown to others; user-editable, follows the hostname by
    /// default
    public let displayName: String
    /// Certificate and private key material, including the fingerprint
    public let material: IdentityMaterial

    /// Certificate fingerprint (the network identity)
    public var fingerprint: String { material.fingerprint }

    /// Load the identity from the data directory; if any of the three identity
    /// files is missing, generate a new identity and persist it
    public static func loadOrCreate(dir: URL) throws -> DeviceIdentity {
        let complete = [metaFile, certFile, keyFile].allSatisfy {
            FileManager.default.fileExists(atPath: dir.appendingPathComponent($0).path)
        }
        return complete ? try load(dir: dir) : try create(dir: dir)
    }

    /// Build the device info exchanged during handshake and discovery
    public func peerInfo() -> PeerInfo {
        PeerInfo(
            deviceId: deviceId, name: displayName, fingerprint: fingerprint,
            platform: "macos", osVersion: osVersionDescription())
    }

    /// Persist the display name (nil restores following the hostname). Only
    /// the metadata changes; the certificate is untouched and the fingerprint
    /// stays. The caller then re-runs loadOrCreate to get a fresh snapshot,
    /// same as on the Rust side.
    public static func persistDisplayName(dir: URL, name: String?) throws {
        let metaURL = dir.appendingPathComponent(metaFile)
        do {
            var meta = try JSONDecoder().decode(IdentityMeta.self, from: Data(contentsOf: metaURL))
            meta.displayName = name
            try writeMeta(meta, to: metaURL)
        } catch {
            throw IdentityError.io(underlying: error)
        }
    }

    /// Load an existing identity; if the metadata is corrupt, rebuild it from
    /// the certificate
    private static func load(dir: URL) throws -> DeviceIdentity {
        do {
            let certDER = try [UInt8](Data(contentsOf: dir.appendingPathComponent(certFile)))
            let keyDER = try [UInt8](Data(contentsOf: dir.appendingPathComponent(keyFile)))
            let material = try IdentityMaterial(certDER: certDER, keyDER: keyDER)
            let metaURL = dir.appendingPathComponent(metaFile)
            let meta: IdentityMeta
            if let parsed = try? JSONDecoder().decode(
                IdentityMeta.self, from: Data(contentsOf: metaURL))
            {
                meta = parsed
            } else {
                // The real device identity is the certificate fingerprint;
                // the metadata can be regenerated
                meta = IdentityMeta(deviceId: UUID().uuidString.lowercased(), displayName: nil)
                try writeMeta(meta, to: metaURL)
            }
            return DeviceIdentity(
                deviceId: meta.deviceId,
                displayName: meta.displayName ?? defaultDisplayName(),
                material: material)
        } catch let error as IdentityError {
            throw error
        } catch {
            throw IdentityError.io(underlying: error)
        }
    }

    /// Generate a new identity (UUID + self-signed P-256 certificate) and
    /// write it to the data directory
    private static func create(dir: URL) throws -> DeviceIdentity {
        let key = P256.Signing.PrivateKey()
        let certDER: [UInt8]
        do {
            certDER = try selfSignedCertDER(for: key)
        } catch {
            throw IdentityError.certGen(underlying: error)
        }
        let deviceId = UUID().uuidString.lowercased()
        do {
            try FileManager.default.createDirectory(
                at: dir, withIntermediateDirectories: true)
            try writeMeta(
                IdentityMeta(deviceId: deviceId, displayName: nil),
                to: dir.appendingPathComponent(metaFile))
            try Data(certDER).write(to: dir.appendingPathComponent(certFile))
            try key.derRepresentation.write(to: dir.appendingPathComponent(keyFile))
            let material = try IdentityMaterial(
                certDER: certDER, keyDER: [UInt8](key.derRepresentation))
            return DeviceIdentity(
                deviceId: deviceId, displayName: defaultDisplayName(), material: material)
        } catch {
            throw IdentityError.io(underlying: error)
        }
    }

    /// Atomic write of the metadata (tmp + rename): an in-place overwrite that
    /// gets interrupted leaves half-written JSON, and the next launch takes
    /// the rebuild path and burns a new device_id for nothing
    private static func writeMeta(_ meta: IdentityMeta, to url: URL) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let bytes = try encoder.encode(meta)
        let tmp = url.appendingPathExtension("tmp")
        try bytes.write(to: tmp)
        _ = try FileManager.default.replaceItemAt(url, withItemAt: tmp)
    }
}

/// Generate a self-signed X.509 certificate (SAN "lanecho"). The contents do
/// not matter — trust rests on the fingerprint alone, and neither side's
/// verification callback looks at validity dates or names.
private func selfSignedCertDER(for key: P256.Signing.PrivateKey) throws -> [UInt8] {
    let certKey = Certificate.PrivateKey(key)
    let name = try DistinguishedName { CommonName("lanecho") }
    let now = Date()
    let certificate = try Certificate(
        version: .v3,
        serialNumber: .init(),
        publicKey: certKey.publicKey,
        notValidBefore: now.addingTimeInterval(-86400),
        notValidAfter: now.addingTimeInterval(86400 * 365 * 100),
        issuer: name,
        subject: name,
        signatureAlgorithm: .ecdsaWithSHA256,
        extensions: try Certificate.Extensions {
            SubjectAlternativeNames([.dnsName("lanecho")])
        },
        issuerPrivateKey: certKey)
    var serializer = DER.Serializer()
    try serializer.serialize(certificate)
    return serializer.serializedBytes
}

/// Default display name: the hostname, falling back to a fixed name when it is
/// unavailable, matching the Rust default_display_name
private func defaultDisplayName() -> String {
    let host = ProcessInfo.processInfo.hostName
    return host.isEmpty ? "Lanecho" : host
}

/// OS version description (e.g. "macOS 15.3.1"); constant for the process
/// lifetime, so it is cached lazily
private let cachedOSVersion: String = {
    let v = ProcessInfo.processInfo.operatingSystemVersion
    return "macOS \(v.majorVersion).\(v.minorVersion).\(v.patchVersion)"
}()

/// OS version for peerInfo
private func osVersionDescription() -> String {
    cachedOSVersion
}
