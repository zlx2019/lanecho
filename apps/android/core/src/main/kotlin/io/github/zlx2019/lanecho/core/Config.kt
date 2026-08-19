package io.github.zlx2019.lanecho.core

// Tuning constants, same names and values as the Rust config.rs / Swift
// Config.swift (docs/PROTOCOL.md §1). Change them there first.

/** Protocol version (major.minor); same major = compatible. */
const val PROTOCOL_VERSION = "1.1"

/** TCP sync port. */
const val DEFAULT_TCP_PORT = 42524

/** UDP multicast discovery port. */
const val DEFAULT_DISCOVERY_PORT = 42525

/** UDP multicast discovery group. */
const val MULTICAST_GROUP = "224.0.0.169"

/** mDNS service type. */
const val MDNS_SERVICE_TYPE = "_lanecho._tcp"

object Config {
    // ---- Discovery ----
    const val HEARTBEAT_INTERVAL_MS = 5_000L
    const val PEER_TIMEOUT_MS = 15_000L
    const val PEER_PROBE_INTERVAL_MS = 30_000L
    const val PEER_PROBE_TIMEOUT_MS = 2_000L

    // ---- Sync engine ----
    const val MAX_SYNC_TEXT_BYTES = 512 * 1024
    const val MAX_SYNC_IMAGE_BYTES = 16L * 1024 * 1024
    const val MAX_SYNC_FILE_COUNT = 64
    const val SYNC_STREAM_BUF = 1024 * 1024
    const val REPLY_TIMEOUT_MS = 30_000L
    const val PAIR_DECISION_TIMEOUT_MS = 300_000L
    const val CONNECT_TIMEOUT_MS = 3_000L

    // ---- Inbound connection governance ----
    const val MAX_CONCURRENT_CONNECTIONS = 64
    const val HANDSHAKE_TIMEOUT_MS = 30_000L
    const val IDLE_TIMEOUT_MS = 60_000L

    /** Drain budget of a graceful close (shutdown → read to EOF). */
    const val GRACEFUL_CLOSE_TIMEOUT_MS = 3_000L
}
