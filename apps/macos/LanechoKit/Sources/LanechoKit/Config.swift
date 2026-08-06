// Tuning constants, centralized (same names and same values as the Rust side's
// deps/lanecho-core/src/config.rs; any change must be mirrored on both sides)

import Foundation

/// Protocol and tuning constants
public enum Config {
    /// Protocol version (compatible when major matches; 1.1 = image/file blob
    /// sync capability)
    public static let protocolVersion = "1.1"

    /// TCP sync port
    public static let tcpPort: UInt16 = 42524
    /// UDP multicast group and port (discovery channel)
    public static let multicastGroup = "224.0.0.169"
    public static let discoveryPort: UInt16 = 42525
    /// mDNS service type (NW API form, without the .local. suffix)
    public static let bonjourType = "_lanecho._tcp"

    /// UDP announce heartbeat interval
    public static let heartbeatInterval: Duration = .seconds(5)
    /// Liveness timeout for UDP-sourced peers (mDNS-sourced peers are never
    /// dropped on a timer)
    public static let peerTimeout: Duration = .seconds(15)
    /// Liveness probe interval for silent peers / per-address timeout
    /// (hard rule: TLS handshake plus fingerprint comparison)
    public static let peerProbeInterval: Duration = .seconds(30)
    public static let peerProbeTimeout: Duration = .seconds(2)

    /// Clipboard poll interval (changeCount fast path: when nothing changed,
    /// only the counter is read)
    public static let watchInterval: Duration = .milliseconds(250)

    /// Per-frame cap (1 MiB): anything larger is refused and the connection
    /// is dropped
    public static let maxFrameLen: UInt32 = 1024 * 1024
    /// Sync text cap (bytes): checked before broadcast and again on the
    /// receiving side
    public static let maxSyncTextBytes = 512 * 1024
    /// Sync image cap (bytes after PNG encoding; same value as the history
    /// image cap, defined independently)
    public static let maxSyncImageBytes: UInt64 = 16 * 1024 * 1024
    /// Blob raw-stream read/write buffer (1 MiB; no per-chunk frame header)
    public static let syncStreamBuf = 1024 * 1024
    /// Maximum number of files in one sync batch
    public static let maxSyncFileCount = 64

    /// TCP connect timeout / peer reply timeout
    public static let connectTimeout: Duration = .seconds(3)
    public static let replyTimeout: Duration = .seconds(30)
    /// Pairing decision timeout (human-in-the-loop prompt)
    public static let pairDecisionTimeout: Duration = .seconds(300)
    /// Overall limit for the unauthenticated phase (TLS plus the Hello gate)
    public static let handshakeTimeout: Duration = .seconds(30)
    /// Inter-frame limit on authenticated connections, so a half-open
    /// connection cannot sit on the quota forever
    public static let idleTimeout: Duration = .seconds(60)
    /// Inbound concurrent connection cap (excess is refused outright, which
    /// guards against slow-loris)
    public static let maxConcurrentConnections = 64

    /// Echo registration ring capacity (one registration per hash)
    public static let echoRecentCap = 8
    /// Lifetime cap of an echo registration (once expired it is reclaimed as
    /// an orphan)
    ///
    /// After the shell layer writes the clipboard, the watcher sees the change
    /// within at most one watchInterval and consumes the registration. But if
    /// the written content is **byte-for-byte identical** to what is already
    /// on the clipboard, the watcher's dedup means the write produces no event
    /// at all — nothing consumes the registration, and once orphaned it
    /// swallows the user's next genuine copy of that same content (no
    /// broadcast, no history entry, no LWW baseline advance).
    ///
    /// **Fixed in the native client first**: the matching defect on the Rust
    /// side is left to a separate pass, so this constant exists only here for
    /// now. That does not violate the spirit of the "same name, same value on
    /// both sides" convention, which covers constants with on-the-wire
    /// semantics.
    public static let echoTTL: Duration = .seconds(2)
    /// Engine event channel capacity
    public static let eventChannelCap = 64
}
