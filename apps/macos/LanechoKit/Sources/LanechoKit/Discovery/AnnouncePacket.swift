// UDP multicast discovery packet.
//
// The JSON shape matches AnnouncePacket in the Rust discovery.rs field for
// field; a peer's IP comes from the UDP source address and is not carried in
// the packet.

import Foundation

/// Multicast packet kind (snake_case on the wire)
public enum AnnounceKind: String, Codable, Sendable {
    /// Periodic broadcast: I am online
    case announce
    /// Unicast reply to an announce, so a new peer sees the existing ones
    /// right away
    case response
    /// Graceful go-offline
    case goodbye
}

/// UDP multicast packet
public struct AnnouncePacket: Codable, Sendable, Equatable {
    /// Packet kind
    public var kind: AnnounceKind
    /// Device info
    public var info: PeerInfo
    /// TCP listening port
    public var tcpPort: UInt16

    enum CodingKeys: String, CodingKey {
        case kind
        case info
        case tcpPort = "tcp_port"
    }

    /// Field-wise initializer
    public init(kind: AnnounceKind, info: PeerInfo, tcpPort: UInt16) {
        self.kind = kind
        self.info = info
        self.tcpPort = tcpPort
    }

    /// Encode to UDP packet bytes
    public func encoded() throws -> Data {
        try JSONEncoder().encode(self)
    }

    /// Decode from UDP packet bytes; anything that is not a lanecho packet
    /// returns nil and is silently ignored
    public static func decoded(from data: Data) -> AnnouncePacket? {
        try? JSONDecoder().decode(AnnouncePacket.self, from: data)
    }
}
