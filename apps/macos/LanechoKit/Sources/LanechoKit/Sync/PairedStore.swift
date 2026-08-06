// Pairing set persistence (paired.json, mirroring the Rust sync/paired.rs)
//
// Pairing is the first gate: a ClipboardSync whose source fingerprint is not in
// this table is rejected.
// Same file name and format as the Rust version: a JSON array with snake_case
// fields.

import Foundation

/// One pairing record; its wire and on-disk shapes match the Rust PairedPeer
public struct PairedPeer: Codable, Sendable, Equatable {
    /// Peer certificate fingerprint (the key)
    public var fingerprint: String
    /// Peer device id
    public var deviceId: String
    /// Display name at pairing time (display only; a peer rename does not
    /// affect the pairing)
    public var name: String
    /// Pairing timestamp (Unix milliseconds)
    public var pairedAtMs: UInt64

    enum CodingKeys: String, CodingKey {
        case fingerprint
        case deviceId = "device_id"
        case name
        case pairedAtMs = "paired_at_ms"
    }
}

/// Pairing set: an in-memory table plus JSON on disk (used inside the engine
/// actor, whose isolation makes access serialized)
struct PairedStore {
    /// On-disk path (paired.json in the data directory)
    private let path: URL
    /// Fingerprint → pairing record
    private var map: [String: PairedPeer] = [:]

    /// Load from the data directory; a missing file or a parse failure is
    /// treated conservatively as an empty table rather than a crash
    init(dir: URL) {
        self.path = dir.appendingPathComponent("paired.json")
        if let bytes = try? Data(contentsOf: path),
            let list = try? JSONDecoder().decode([PairedPeer].self, from: bytes)
        {
            self.map = Dictionary(uniqueKeysWithValues: list.map { ($0.fingerprint, $0) })
        }
    }

    /// Whether the fingerprint is paired
    func contains(_ fingerprint: String) -> Bool {
        map[fingerprint] != nil
    }

    /// Insert a pairing (idempotent; refreshes the name when it already exists)
    /// and persist
    mutating func insert(_ info: PeerInfo) {
        map[info.fingerprint] = PairedPeer(
            fingerprint: info.fingerprint, deviceId: info.deviceId,
            name: info.name, pairedAtMs: nowMs())
        persist()
    }

    /// Remove a pairing and persist; returns whether it existed
    mutating func remove(_ fingerprint: String) -> Bool {
        guard map.removeValue(forKey: fingerprint) != nil else { return false }
        persist()
        return true
    }

    /// All pairing records, ordered by name for a stable output that matches
    /// the Rust list
    func list() -> [PairedPeer] {
        map.values.sorted {
            ($0.name, $0.fingerprint) < ($1.name, $1.fingerprint)
        }
    }

    /// Atomic write (tmp + rename); a failure is only reported — the in-memory
    /// table still applies.
    /// Callers run inside the engine actor, whose isolation serializes writes
    /// (the counterpart of the Rust io lock).
    private func persist() {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let bytes = try encoder.encode(list())
            let tmp = path.appendingPathExtension("tmp")
            try bytes.write(to: tmp)
            _ = try FileManager.default.replaceItemAt(path, withItemAt: tmp)
        } catch {
            // A failed write must not take the pairing operation down with it
            // (the Rust side makes the same trade-off)
        }
    }
}
