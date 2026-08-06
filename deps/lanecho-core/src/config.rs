//! Engine tuning constants: heartbeat, timeout, polling and other scattered
//! parameters, defined in one place.
//!
//! One place to look them up, one place to change them; should runtime
//! configuration ever be needed (settings page / CLI flags), this module is
//! the field list to grow into an injectable config struct, with the
//! constants becoming its defaults.
//!
//! Ports and protocol-side limits do not live here: port defaults
//! (`DEFAULT_TCP_PORT` and friends) sit at the crate root, and the frame size
//! limit is a contract both ends agree on, defined in [`crate::protocol`].

use std::time::Duration;

// ---- Discovery ----

/// Heartbeat interval: the period of the UDP multicast announce
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Peer timeout: no heartbeat for this long means the peer goes offline
/// (tolerates 2 consecutive missed heartbeats)
pub const PEER_TIMEOUT: Duration = Duration::from_secs(15);

/// Liveness probe interval for crashed peers: a peer that is "alive on mDNS
/// but silent on UDP" gets one TCP probe per interval
pub const PEER_PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Total per-address budget for one liveness probe (covers TCP connect plus
/// the TLS handshake; on a LAN the handshake takes milliseconds, so 2s leaves
/// room for both phases)
pub const PEER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Event channel capacity. Reused in two places with different overflow
/// semantics:
/// - discovery peer events: `try_send` drops when full, consumers can fall
///   back to a snapshot;
/// - sync engine events: `send().await` applies backpressure, so a stalled
///   consumer (the desktop event pump) hangs the sender entirely — the
///   consumer must keep draining and must not do slow work
pub const EVENT_CHANNEL_CAP: usize = 64;

// ---- Clipboard watcher ----

/// Change-stamp polling period (macOS changeCount / Windows sequence number,
/// both a single cheap syscall): 250ms cuts the worst-case "copy → pastable
/// on the peer" detection delay to a quarter second, and doubling the call
/// rate is negligible at microsecond cost
pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// Polling period for the Linux fallback path (no cheap change stamp, and
/// reading text to compare costs more, so the period is relaxed)
pub const WATCH_INTERVAL_FALLBACK: Duration = Duration::from_secs(1);

// ---- Sync engine ----

/// Text payload cap for cross-device sync (512 KiB): leaves room within the
/// 1MiB frame limit for JSON escaping and metadata; oversized content is not
/// broadcast (logged instead), and local history is not subject to this cap
pub const MAX_SYNC_TEXT_BYTES: usize = 512 * 1024;

/// Image payload cap for cross-device sync (16MB after PNG encoding): same
/// value as the local history image cap but defined separately — the history
/// constant belongs to the storage pipeline, and the two do not share a
/// symbol because their meanings differ
pub const MAX_SYNC_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// File count cap for a single file sync: transfer is manifest-driven and
/// streams file by file, so an overly long manifest (a hundred thousand empty
/// files) would make the receiver create a mountain of files — a normal
/// clipboard never comes close
pub const MAX_SYNC_FILE_COUNT: usize = 64;

/// Read/write buffer for the raw blob byte stream (1 MiB); the stream has no
/// per-chunk frame header, so this is only IO granularity — do not confuse it
/// with the protocol frame limit
pub const SYNC_STREAM_BUF: usize = 1024 * 1024;

/// How long to wait for handshake and reply messages
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout waiting for the peer's pairing decision (a human is in the loop,
/// hence the long timeout)
pub const PAIR_DECISION_TIMEOUT: Duration = Duration::from_secs(300);

/// TCP connect timeout per candidate address (addresses are tried one by one
/// across interfaces, so this must stay short)
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Capacity of the echo registration ring for recent remote writes (a ring of
/// content hashes; 1-2 entries suffice normally, the rest absorbs the pile-up
/// when several syncs land back to back before polling catches up)
pub const ECHO_RECENT_CAP: usize = 8;

/// Lifetime cap for an echo registration (past it the entry is reclaimed as
/// an orphan)
///
/// The **only** consumer of a registration is a change event from the
/// watcher, and the watcher dedups by skipping unchanged content (the
/// `last_hash` branch in [`crate::clipboard`]) — if the written text is
/// byte-for-byte identical to what the clipboard already holds, the write
/// produces no event at all and nobody consumes the registration. An orphan
/// then swallows the user's next **genuine copy of that same text** whole (no
/// broadcast, no history entry, no LWW baseline advance), and it takes
/// [`ECHO_RECENT_CAP`] further remote syncs to push it out of the ring, so in
/// practice it can linger for hours.
///
/// 4s rather than something shorter: the Linux fallback path polls every 1s
/// (see [`WATCH_INTERVAL_FALLBACK`]), so a normal round trip waits at most one
/// tick and this leaves four ticks of headroom.
pub const ECHO_TTL: Duration = Duration::from_secs(4);

// ---- Inbound connection governance (receiver) ----

/// Concurrent connection cap: new connections beyond it are rejected outright
/// (keeps slow-loris from exhausting fds/memory)
///
/// A lanecho connection is dial, one frame, done — there is no long-lived
/// data stream, so 64 is generous.
pub const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// Timeout for the unauthenticated phase (TLS handshake + first frame), which
/// blocks connections that squat after connecting without saying anything
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Inter-frame timeout on an authenticated connection: transactions complete
/// in milliseconds, so a silently hung half-open connection (peer asleep or
/// unplugged) must not hold a connection slot for long — 64 half-open
/// connections would paralyze all inbound traffic
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
