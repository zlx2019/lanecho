// Device info, shared by the handshake and discovery.
//
// The JSON shape matches PeerInfo in the Rust protocol.rs field for field:
// snake_case keys, and os_version is not serialized when absent — older
// versions are allowed to omit it.

import Foundation

/// Device info
public struct PeerInfo: Codable, Sendable, Equatable, Hashable {
    /// Unique device ID (UUID)
    public var deviceId: String
    /// Display name
    public var name: String
    /// Certificate BLAKE3 fingerprint (64 lowercase hex characters)
    public var fingerprint: String
    /// Platform identifier (macos/windows/linux)
    public var platform: String
    /// OS version description (e.g. "macOS 15.3"); optional, older versions
    /// may omit it
    public var osVersion: String?

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case name
        case fingerprint
        case platform
        case osVersion = "os_version"
    }

    /// Field-wise initializer
    public init(
        deviceId: String, name: String, fingerprint: String,
        platform: String, osVersion: String? = nil
    ) {
        self.deviceId = deviceId
        self.name = name
        self.fingerprint = fingerprint
        self.platform = platform
        self.osVersion = osVersion
    }
}
