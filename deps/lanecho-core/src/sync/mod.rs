//! Sync engine: pairing model, group broadcast, LWW resolution, echo
//! suppression.
//!
//! The engine never touches the system clipboard directly:
//! - Input is a [`ClipboardEvent`] stream — the production shell wires up
//!   [`crate::clipboard::spawn_watcher`], loopback tests inject events
//!   directly;
//! - A remote sync is landed by the shell layer, which applies the
//!   [`EngineEvent::ApplyRemote`] event; the echo hash is registered
//!   **before** that event is emitted (the write precedes the watcher's
//!   detection, so the ordering is guaranteed).
//!
//! Echo suppression is a hard rule: content written by a remote sync is never
//! broadcast; "copy this entry" from the history panel is explicit user
//! intent and broadcasts normally through the regular copy path.

mod blob;
mod net;
mod paired;

pub use blob::SYNC_FILES_DIR;
pub use paired::PairedPeer;

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::clipboard::{ClipboardContent, ClipboardEvent, hash_text, now_ms};
use crate::config::{
    ECHO_RECENT_CAP, ECHO_TTL, EVENT_CHANNEL_CAP, MAX_SYNC_FILE_COUNT, MAX_SYNC_IMAGE_BYTES,
    MAX_SYNC_TEXT_BYTES, PAIR_DECISION_TIMEOUT,
};
use crate::discovery::{DiscoveryError, DiscoveryService, Peer, PeerEvent};
use crate::identity::{DeviceIdentity, IdentityError};
use crate::protocol::{
    self, ControlMessage, FileMeta, FilesOfferMeta, ImageOfferMeta, PeerInfo, ProtocolError,
    content_type, reason_code,
};
use crate::tls::{self, TlsError};

/// Sync engine errors
#[derive(Debug, Error)]
pub enum SyncError {
    /// Discovery layer error
    #[error("发现服务错误: {0}")]
    Discovery(#[from] DiscoveryError),
    /// Identity layer error
    #[error("设备身份错误: {0}")]
    Identity(#[from] IdentityError),
    /// TLS configuration error
    #[error("TLS 错误: {0}")]
    Tls(#[from] TlsError),
    /// Protocol layer error (codec, version, out-of-order frame)
    #[error("协议错误: {0}")]
    Protocol(#[from] ProtocolError),
    /// Underlying IO error
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// Peer unreachable: offline, or every candidate address failed to connect
    #[error("对端不可达")]
    PeerUnreachable,
    /// The peer's TLS certificate disagrees with the fingerprint it declared
    /// (possible impersonation)
    #[error("对端指纹与声明不一致")]
    FingerprintMismatch,
    /// The remote user rejected the pairing request
    #[error("对端拒绝配对")]
    PairRejected,
    /// The peer rejected the sync; the payload is a structured reason code
    #[error("对端拒绝同步: {0}")]
    Rejected(String),
    /// Timed out waiting for the peer's reply; the payload names the step
    #[error("等待 {0} 超时")]
    Timeout(&'static str),
}

/// Sync direction policy: send and receive are gated independently
///
/// Externally (settings.json) this is a string; falling back to
/// [`SyncMode::Both`] when parsing fails is the shell layer's job. Inside the
/// engine it is split into two atomic booleans, send and recv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Fully off: local clipboard history only
    Off,
    /// Sync both ways (default)
    Both,
    /// Send only: broadcast local copies, refuse inbound syncs
    Send,
    /// Receive only: never broadcast, only accept inbound syncs
    Receive,
}

impl SyncMode {
    /// Parse from the settings string; an unknown value returns None and the
    /// caller decides the fallback
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "both" => Some(Self::Both),
            "send" => Some(Self::Send),
            "receive" => Some(Self::Receive),
            _ => None,
        }
    }

    /// The settings string form (the value stored in settings.json)
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Both => "both",
            Self::Send => "send",
            Self::Receive => "receive",
        }
    }

    /// Whether local copies may be broadcast
    fn sends(&self) -> bool {
        matches!(self, Self::Both | Self::Send)
    }

    /// Whether inbound syncs are accepted
    fn receives(&self) -> bool {
        matches!(self, Self::Both | Self::Receive)
    }
}

/// Engine configuration
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Engine data directory (holds the identity file and paired.json)
    pub data_dir: PathBuf,
    /// TCP listen port (0 = pick a free one; for tests and for several
    /// instances on one machine)
    pub tcp_port: u16,
    /// UDP multicast discovery port
    pub discovery_port: u16,
    /// Discovery-layer stealth (listen only, never announce)
    pub passive: bool,
    /// Initial sync direction policy (switchable at runtime)
    pub sync_mode: SyncMode,
    /// Initial sync type toggles (text/images/files; one toggle covers both
    /// directions, switchable at runtime)
    pub sync_types: SyncTypes,
    /// Cap on the total bytes of a file sync (switchable at runtime)
    pub max_sync_file_bytes: u64,
}

/// Sync type toggles: only the enabled types are sent and received
#[derive(Debug, Clone, Copy)]
pub struct SyncTypes {
    /// Text
    pub text: bool,
    /// Images
    pub images: bool,
    /// Files
    pub files: bool,
}

impl Default for SyncTypes {
    fn default() -> Self {
        // Files are off by default: they involve writing to disk and cleanup
        Self {
            text: true,
            images: true,
            files: false,
        }
    }
}

/// Engine events: the only source of information for the UI and the CLI
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A peer came online, or its info changed
    PeerUp(Peer),
    /// A peer went offline; the payload is its fingerprint
    PeerDown(String),
    /// An inbound pairing request arrived; the layer above prompts the user
    /// and feeds the decision back via [`SyncEngine::respond_pair`]
    PairRequested {
        /// Device info of the requester
        peer: PeerInfo,
    },
    /// Pairing established, in either direction: a successful outbound pair
    /// and an accepted inbound one both fire this
    Paired {
        /// Peer device info
        peer: PeerInfo,
    },
    /// Pairing removed, either locally or on notice from the peer
    Unpaired {
        /// Peer fingerprint
        fingerprint: String,
    },
    /// The local user copied something new (the history pipeline and the UI
    /// hints hang off this event)
    LocalCopied {
        /// Clipboard content
        content: ClipboardContent,
        /// Content hash
        hash: String,
        /// Copy timestamp (Unix milliseconds)
        timestamp_ms: u64,
    },
    /// A remote sync was accepted; the shell layer should write the content to
    /// the system clipboard
    ///
    /// **Contract**: the engine registers the echo hash before emitting this
    /// event, assuming the write will happen. If the write fails, the shell
    /// layer must call [`SyncEngine::cancel_echo`] to undo the registration;
    /// otherwise the leftover orphan hash swallows the next genuine local copy
    /// of the same content.
    ///
    /// Since 1.1 content covers three kinds: text byte-for-byte as received;
    /// images as decoded RGBA (the echo hash is the RGBA hash, the same basis
    /// history dedup uses); files as **local landing paths** (under
    /// sync-files, with the echo hash computed over those paths — the peer's
    /// paths are meaningless here).
    ApplyRemote {
        /// The synced content
        content: ClipboardContent,
        /// Source device
        from: PeerInfo,
        /// The peer's copy timestamp (Unix milliseconds)
        timestamp_ms: u64,
        /// The echo hash already registered; pass it back to cancel on a
        /// failed write
        hash: String,
    },
    /// The outcome of one outbound sync, reported per target peer
    SyncSent {
        /// Target device
        to: PeerInfo,
        /// Ok means delivered; Err carries the reason code or an error
        /// description
        result: Result<(), String>,
    },
}

/// A parsed blob offer (net.rs decodes it out of clipboard_sync.data)
pub(crate) enum BlobOffer {
    /// Image (PNG stream)
    Image(ImageOfferMeta),
    /// File batch
    Files(FilesOfferMeta),
}

/// The verdict on a blob offer; three branches
pub(crate) enum OfferDecision {
    /// Wanted: reply BlobAccept and have the peer send the stream
    Accept,
    /// LWW says stale: reply SyncAck and the peer goes straight to Bye
    Stale,
    /// Refused: reply SyncRejected(code)
    Reject(&'static str),
}

/// A pending pairing request: the decision channel plus the requester's info
/// (the latter is what the startup fallback pulls)
///
/// `generation` tells concurrent requests from the same fingerprint apart:
/// when a later request displaces an earlier one, the earlier one's cleanup
/// must remove only its own generation and not the handle the displacing
/// request just inserted.
struct PendingPair {
    /// Request generation (monotonically increasing)
    generation: u64,
    /// Device info of the requester
    peer: PeerInfo,
    /// Decision channel (the UI feeds it through respond_pair)
    tx: oneshot::Sender<bool>,
}

/// Shared engine state, used by the network sessions and every pump task
pub(crate) struct Inner {
    /// Local identity. Mutex<Arc> so a rename can swap the snapshot — the
    /// fingerprint and certificate stay put, only the display name moves
    identity: Mutex<Arc<DeviceIdentity>>,
    /// Engine data directory (identity and pairing files live here; also
    /// where a rename is persisted)
    data_dir: PathBuf,
    /// Server TLS config, shared by every inbound connection; the certificate
    /// survives a rename, so it never needs rebuilding
    pub(crate) server_tls: Arc<rustls::ServerConfig>,
    /// Discovery service
    discovery: DiscoveryService,
    /// The pairing set
    paired: Mutex<paired::PairedStore>,
    /// Serializes writes of the pairing table to disk; the snapshot is taken
    /// inside the lock so concurrent writes cannot land out of order and roll
    /// state back
    paired_io: Arc<tokio::sync::Mutex<()>>,
    /// Engine event sender
    events: mpsc::Sender<EngineEvent>,
    /// Monotonic sequence number for outbound syncs (for log tracing)
    seq: AtomicU64,
    /// Timestamp of the last local copy (Unix milliseconds); the LWW baseline
    last_local_copy_ms: AtomicU64,
    /// Content hashes of recent remote writes and when they were registered
    /// (echo suppression, consumed once)
    ///
    /// The registration time is what makes orphan reclamation possible: the
    /// only consumer is a watcher event, and when the written content equals
    /// what is already on the clipboard the watcher dedups it and no event is
    /// ever produced. See [`ECHO_TTL`]
    echo: Mutex<VecDeque<(String, u64)>>,
    /// Direction gate: send (whether local copies are broadcast)
    send_enabled: AtomicBool,
    /// Direction gate: receive (whether inbound syncs are accepted)
    recv_enabled: AtomicBool,
    /// Type gate: text
    sync_text: AtomicBool,
    /// Type gate: images
    sync_images: AtomicBool,
    /// Type gate: files
    sync_files: AtomicBool,
    /// Cap on the total bytes of a file sync
    max_sync_file_bytes: AtomicU64,
    /// Inbound pairing requests waiting on a UI decision (fingerprint →
    /// pending record)
    pending_pairs: Mutex<HashMap<String, PendingPair>>,
    /// Pairing request generation counter (see [`PendingPair::generation`])
    pending_seq: AtomicU64,
    /// Client TLS config cache, keyed by peer fingerprint: rebuilding one per
    /// dial re-parses the local certificate and private key DER every time.
    /// Certificates and fingerprints do not change while the process runs (a
    /// rename only touches the display name), so entries never expire; a
    /// config left behind after unpair is harmless, and the cache is bounded
    /// by the number of peers ever paired
    client_tls: Mutex<HashMap<String, Arc<rustls::ClientConfig>>>,
    /// The port actually bound
    port: u16,
}

impl Inner {
    /// Engine data directory (where received blobs land)
    pub(crate) fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Snapshot of the current identity (an Arc clone, cheap)
    pub(crate) fn current_identity(&self) -> Arc<DeviceIdentity> {
        Arc::clone(&self.identity.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Lock the pairing table, recovering the data if the lock was poisoned
    fn lock_paired(&self) -> MutexGuard<'_, paired::PairedStore> {
        self.paired.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock the echo table
    fn lock_echo(&self) -> MutexGuard<'_, VecDeque<(String, u64)>> {
        self.echo.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock the pending pairing table
    fn lock_pending(&self) -> MutexGuard<'_, HashMap<String, PendingPair>> {
        self.pending_pairs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Client TLS config for a given peer, cached (see [`Inner::client_tls`])
    ///
    /// Two concurrent misses may build it twice; the later write wins and that
    /// is harmless.
    fn client_tls_for(&self, fingerprint: &str) -> Result<Arc<rustls::ClientConfig>, SyncError> {
        if let Some(config) = self
            .client_tls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(fingerprint)
        {
            return Ok(Arc::clone(config));
        }
        let config = Arc::new(tls::client_config(
            &self.current_identity(),
            Some(fingerprint.to_string()),
        )?);
        self.client_tls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(fingerprint.to_string(), Arc::clone(&config));
        Ok(config)
    }

    /// Emit an engine event; silently dropped once the consumer is gone (the
    /// engine is shut down right after)
    pub(crate) async fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event).await;
    }

    /// Whether this fingerprint is paired
    fn is_paired(&self, fingerprint: &str) -> bool {
        self.lock_paired().contains(fingerprint)
    }

    /// Every paired fingerprint (used to filter broadcast targets)
    fn paired_fingerprints(&self) -> HashSet<String> {
        self.lock_paired()
            .list()
            .into_iter()
            .map(|p| p.fingerprint)
            .collect()
    }

    /// Record a pairing (idempotent), persist it, emit the event
    pub(crate) async fn add_paired(self: &Arc<Self>, info: &PeerInfo) {
        self.lock_paired().insert(info);
        self.persist_paired().await;
        self.emit(EngineEvent::Paired { peer: info.clone() }).await;
    }

    /// Remove a pairing, persist, emit the event; a no-op if it was not there
    pub(crate) async fn remove_paired(self: &Arc<Self>, fingerprint: &str) {
        if !self.lock_paired().remove(fingerprint) {
            return;
        }
        self.persist_paired().await;
        self.emit(EngineEvent::Unpaired {
            fingerprint: fingerprint.to_string(),
        })
        .await;
    }

    /// Persist the pairing table: serialized IO, snapshot taken inside the lock
    ///
    /// If concurrent changes each carried their own snapshot to disk, the
    /// completion order would be unspecified and an older snapshot could
    /// overwrite a newer one — worst case, an unpaired peer comes back to life
    /// after a restart, rolling back the receive-side security boundary.
    /// Taking the snapshot inside the serializing lock keeps what reaches disk
    /// moving forward only.
    async fn persist_paired(self: &Arc<Self>) {
        let inner = Arc::clone(self);
        let guard = Arc::clone(&self.paired_io).lock_owned().await;
        let joined = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let (path, list) = {
                let store = inner.lock_paired();
                (store.path(), store.list())
            };
            paired::write_snapshot(&path, &list);
        })
        .await;
        if joined.is_err() {
            tracing::warn!("配对表落盘任务中断(内存态仍生效)");
        }
    }

    /// Inbound pairing decision: an already-paired peer is accepted
    /// idempotently, otherwise the request goes up to the UI and waits on the
    /// user
    ///
    /// On concurrent duplicate requests from one peer the later displaces the
    /// earlier, whose waiter sees the channel close and treats that as a
    /// rejection; the earlier one's cleanup uses the generation to delete only
    /// its own entry.
    pub(crate) async fn decide_pair(self: &Arc<Self>, remote: &PeerInfo) -> bool {
        if self.is_paired(&remote.fingerprint) {
            return true;
        }
        let generation = self.pending_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        // Do not notify again when this peer already has a pending request
        // (the displacement case): every PairRequested raises a system
        // notification in the shell layer, so a peer that keeps reconnecting
        // and re-sending PairRequest — a broken retry loop or a malicious one
        // — could flood Notification Center. The pending table still
        // displaces as usual; the startup fallback list_pending_pairs reads
        // that table rather than the event stream, so it is unaffected
        let already_prompted = self
            .lock_pending()
            .insert(
                remote.fingerprint.clone(),
                PendingPair {
                    generation,
                    peer: remote.clone(),
                    tx,
                },
            )
            .is_some();
        if !already_prompted {
            self.emit(EngineEvent::PairRequested {
                peer: remote.clone(),
            })
            .await;
        }
        let accepted = matches!(
            tokio::time::timeout(PAIR_DECISION_TIMEOUT, rx).await,
            Ok(Ok(true))
        );
        {
            // Clean up only our own generation: once a concurrent request has
            // displaced us, the table holds the other side's handle
            let mut pending = self.lock_pending();
            if pending
                .get(&remote.fingerprint)
                .is_some_and(|p| p.generation == generation)
            {
                pending.remove(&remote.fingerprint);
            }
        }
        if accepted {
            self.add_paired(remote).await;
        }
        accepted
    }

    /// Inbound sync check chain: kill switch → paired → type → size → LWW
    ///
    /// Once it passes, **the echo hash is registered before ApplyRemote is
    /// emitted**: the shell layer writes the clipboard before the watcher's
    /// next poll, and that ordering is what guarantees the echo gets
    /// swallowed. When LWW says ignore we still accept and reply Ok — the peer
    /// has no need to tell "applied" apart from "not applied because the local
    /// clipboard is newer".
    pub(crate) async fn accept_sync(
        &self,
        remote: &PeerInfo,
        timestamp_ms: u64,
        content_kind: &str,
        data: String,
    ) -> Result<(), &'static str> {
        if !self.recv_enabled.load(Ordering::Relaxed) {
            return Err(reason_code::DISABLED);
        }
        if !self.is_paired(&remote.fingerprint) {
            return Err(reason_code::NOT_PAIRED);
        }
        // Images and files never reach this function (net.rs routes them to
        // blob handling by content_type); anything non-text arriving here is
        // an unknown new type, so always answer unsupported — a guard rail for
        // protocol evolution
        if content_kind != content_type::TEXT || !self.sync_text.load(Ordering::Relaxed) {
            return Err(reason_code::UNSUPPORTED_TYPE);
        }
        if data.len() > MAX_SYNC_TEXT_BYTES {
            return Err(reason_code::TOO_LARGE);
        }
        // LWW: ignore when the local copy is newer, ties included — one fewer
        // overwrite is the conservative choice. Logged at info with both
        // timestamps: clock drift shows up as one side permanently refusing
        // everything, and this leaves a trail
        let local_ms = self.last_local_copy_ms.load(Ordering::Relaxed);
        if timestamp_ms <= local_ms {
            tracing::info!(
                from = %remote.name,
                remote_ms = timestamp_ms,
                local_ms,
                "LWW: 本机剪贴板更新, 忽略远端同步(频繁出现时检查双方时钟)"
            );
            return Ok(());
        }
        let hash = hash_text(&data);
        self.push_echo(hash.clone());
        self.emit(EngineEvent::ApplyRemote {
            content: ClipboardContent::Text(data),
            from: remote.clone(),
            timestamp_ms,
            hash,
        })
        .await;
        Ok(())
    }

    /// Verdict on a blob offer, decided in one pass **before any stream is
    /// read**: direction → paired → type toggle → size/count → LWW.
    /// A single-frame design has to receive everything before it can refuse;
    /// deciding at the offer stage stops transfers that were doomed anyway.
    pub(crate) fn decide_blob_offer(
        &self,
        remote: &PeerInfo,
        timestamp_ms: u64,
        offer: &BlobOffer,
    ) -> OfferDecision {
        if !self.recv_enabled.load(Ordering::Relaxed) {
            return OfferDecision::Reject(reason_code::DISABLED);
        }
        if !self.is_paired(&remote.fingerprint) {
            return OfferDecision::Reject(reason_code::NOT_PAIRED);
        }
        match offer {
            BlobOffer::Image(meta) => {
                if !self.sync_images.load(Ordering::Relaxed) {
                    return OfferDecision::Reject(reason_code::UNSUPPORTED_TYPE);
                }
                if meta.total_bytes == 0 || meta.total_bytes > MAX_SYNC_IMAGE_BYTES {
                    return OfferDecision::Reject(reason_code::TOO_LARGE);
                }
            }
            BlobOffer::Files(meta) => {
                if !self.sync_files.load(Ordering::Relaxed) {
                    return OfferDecision::Reject(reason_code::UNSUPPORTED_TYPE);
                }
                let limit = self.max_sync_file_bytes.load(Ordering::Relaxed);
                // The manifest comes from the peer, so the sum must be
                // checked: a malicious declaration of u64::MAX+2 makes a bare
                // sum panic outright in a debug build, remotely triggerable
                let Some(declared) = meta
                    .files
                    .iter()
                    .try_fold(0u64, |acc, f| acc.checked_add(f.bytes))
                else {
                    return OfferDecision::Reject(reason_code::TOO_LARGE);
                };
                if meta.files.is_empty()
                    || meta.files.len() > MAX_SYNC_FILE_COUNT
                    || meta.total_bytes == 0
                    || meta.total_bytes > limit
                    // The manifest must sum to the declared total: the
                    // receiver splits the stream by the manifest, so a
                    // mismatch means the stream length is wrong and receiving
                    // it is pointless
                    || declared != meta.total_bytes
                {
                    return OfferDecision::Reject(reason_code::TOO_LARGE);
                }
            }
        }
        // LWW up front: reply Ack when stale, keeping the text-path semantics
        // that the peer need not distinguish the two cases; a dialer seeing
        // Ack instead of BlobAccept knows there is nothing to transfer
        let local_ms = self.last_local_copy_ms.load(Ordering::Relaxed);
        if timestamp_ms <= local_ms {
            tracing::info!(
                from = %remote.name,
                remote_ms = timestamp_ms,
                local_ms,
                "LWW: 本机剪贴板更新, blob offer 无需传输"
            );
            return OfferDecision::Stale;
        }
        OfferDecision::Accept
    }

    /// Land an image blob once its stream is fully received: decode → register
    /// the echo → ApplyRemote
    ///
    /// On Err (a reason code) the caller replies SyncRejected. Decoding runs
    /// on spawn_blocking — PNG decoding is pure CPU work and must not tie up
    /// an async thread.
    pub(crate) async fn finish_blob_image(
        &self,
        remote: &PeerInfo,
        timestamp_ms: u64,
        png: Vec<u8>,
    ) -> Result<(), &'static str> {
        let decoded = tokio::task::spawn_blocking(move || blob::decode_png_rgba(&png)).await;
        let Ok(Ok((width, height, rgba))) = decoded else {
            tracing::info!(from = %remote.name, "远端图像 PNG 解码失败, 拒绝");
            return Err(reason_code::UNSUPPORTED_TYPE);
        };
        let content = ClipboardContent::Image {
            width,
            height,
            rgba,
        };
        let hash = content.hash();
        self.push_echo(hash.clone());
        self.emit(EngineEvent::ApplyRemote {
            content,
            from: remote.clone(),
            timestamp_ms,
            hash,
        })
        .await;
        Ok(())
    }

    /// Land a file blob once its stream is fully received: name the `.part`
    /// files → register the echo → ApplyRemote
    ///
    /// The echo hash is computed over the **local landing paths** — those are
    /// exactly what the shell layer puts on the clipboard and what the watcher
    /// reads back on the way around; the peer's paths mean nothing here.
    pub(crate) async fn finish_blob_files(
        &self,
        remote: &PeerInfo,
        timestamp_ms: u64,
        parts: Vec<PathBuf>,
        metas: &[FileMeta],
        batch_dir: &std::path::Path,
    ) -> Result<(), &'static str> {
        let Ok(finals) = blob::finalize_parts(&parts, metas, batch_dir).await else {
            tracing::warn!(from = %remote.name, "远端文件定名失败, 拒绝");
            return Err(reason_code::UNSUPPORTED_TYPE);
        };
        let content = ClipboardContent::Files(finals);
        let hash = content.hash();
        self.push_echo(hash.clone());
        self.emit(EngineEvent::ApplyRemote {
            content,
            from: remote.clone(),
            timestamp_ms,
            hash,
        })
        .await;
        Ok(())
    }

    /// Register the echo hash of one remote write; evicts the oldest when full
    ///
    /// A hash is registered only once: the watcher dedups identical content
    /// and produces a single echo event, so registering twice (several peers
    /// syncing the same text in turn) leaves an orphan behind that swallows a
    /// later genuine local copy.
    fn push_echo(&self, hash: String) {
        let mut echo = self.lock_echo();
        Self::prune_echo(&mut echo);
        if echo.iter().any(|(h, _)| *h == hash) {
            return;
        }
        if echo.len() >= ECHO_RECENT_CAP {
            echo.pop_front();
        }
        echo.push_back((hash, now_ms()));
    }

    /// Consume an echo on hit (one shot): true means the caller should skip
    /// this clipboard event
    fn take_echo(&self, hash: &str) -> bool {
        let mut echo = self.lock_echo();
        Self::prune_echo(&mut echo);
        match echo.iter().position(|(h, _)| h == hash) {
            Some(idx) => {
                echo.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Reclaim expired echo registrations, i.e. orphans (see [`ECHO_TTL`])
    ///
    /// Guard the comparison instead of subtracting outright: when the system
    /// clock jumps backwards `now < at`, and unsigned subtraction underflows
    /// into an astronomical number that clears **every** registration at once
    /// — remote content would then be taken for a local copy and broadcast
    /// straight back, ping-ponging overwrites. Better to reclaim late than to
    /// reclaim wrongly.
    fn prune_echo(echo: &mut VecDeque<(String, u64)>) {
        let now = now_ms();
        let ttl = ECHO_TTL.as_millis() as u64;
        echo.retain(|(_, at)| !(now > *at && now - *at > ttl));
    }

    /// Broadcast one piece of local text to every peer that is both paired and
    /// online, dialing them concurrently
    ///
    /// The sync frame is byte-for-byte identical for every target: serialize
    /// once and share it as `Arc<[u8]>` rather than cloning the whole text per
    /// peer and JSON-encoding it again for each. Exceeding the frame cap (JSON
    /// escaping inflates by up to 6x in the worst case) is refused during
    /// encoding, so it never fails only after dialing.
    async fn broadcast_text(self: &Arc<Self>, text: String, timestamp_ms: u64) {
        let paired = self.paired_fingerprints();
        if paired.is_empty() {
            return;
        }
        let targets: Vec<Peer> = self
            .discovery
            .peers()
            .into_iter()
            .filter(|p| paired.contains(&p.info.fingerprint))
            .collect();
        if targets.is_empty() {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let bytes = text.len();
        let frame: Arc<[u8]> = match protocol::encode_frame(&ControlMessage::ClipboardSync {
            seq,
            timestamp_ms,
            content_type: content_type::TEXT.to_string(),
            data: text,
        }) {
            Ok(frame) => frame.into(),
            Err(e) => {
                tracing::info!(
                    bytes,
                    "文本编码后超过协议帧上限, 不广播(本机历史不受影响): {e}"
                );
                return;
            }
        };
        for peer in targets {
            let inner = Arc::clone(self);
            let frame = Arc::clone(&frame);
            tokio::spawn(async move {
                let identity = inner.current_identity();
                let result = match inner.client_tls_for(&peer.info.fingerprint) {
                    Ok(config) => net::sync_transaction(&identity, config, &peer, &frame)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                inner
                    .emit(EngineEvent::SyncSent {
                        to: peer.info,
                        result,
                    })
                    .await;
            });
        }
    }
}

/// Data source for a blob broadcast; shared across the per-peer transactions
/// rather than cloned for each
#[derive(Clone)]
pub(crate) enum BlobSource {
    /// In-memory bytes (image PNG)
    Memory(Arc<[u8]>),
    /// Files on disk; each peer streams them from disk on its own — with the
    /// usual one or two peers a shared read is not worth it
    Files(Arc<Vec<PathBuf>>),
}

impl Inner {
    /// Broadcast one blob (image or files) to every peer that is both paired
    /// and online
    ///
    /// The offer frame is encoded once and shared; every peer runs its own
    /// concurrent transaction, one peer failing does not affect the others,
    /// and a SyncSent event is emitted per peer — the same shape as the text
    /// broadcast.
    async fn broadcast_blob(
        self: &Arc<Self>,
        kind: &'static str,
        meta_json: String,
        metas: Arc<Vec<FileMeta>>,
        source: BlobSource,
        timestamp_ms: u64,
    ) {
        let paired = self.paired_fingerprints();
        if paired.is_empty() {
            return;
        }
        let targets: Vec<Peer> = self
            .discovery
            .peers()
            .into_iter()
            .filter(|p| paired.contains(&p.info.fingerprint))
            .collect();
        if targets.is_empty() {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let frame: Arc<[u8]> = match protocol::encode_frame(&ControlMessage::ClipboardSync {
            seq,
            timestamp_ms,
            content_type: kind.to_string(),
            data: meta_json,
        }) {
            Ok(frame) => frame.into(),
            Err(e) => {
                tracing::warn!("blob offer 编码失败, 不广播: {e}");
                return;
            }
        };
        for peer in targets {
            let inner = Arc::clone(self);
            let frame = Arc::clone(&frame);
            let metas = Arc::clone(&metas);
            let source = source.clone();
            tokio::spawn(async move {
                let identity = inner.current_identity();
                let result = match inner.client_tls_for(&peer.info.fingerprint) {
                    Ok(config) => {
                        net::blob_transaction(&identity, config, &peer, &frame, &metas, source)
                            .await
                            .map_err(|e| e.to_string())
                    }
                    Err(e) => Err(e.to_string()),
                };
                inner
                    .emit(EngineEvent::SyncSent {
                        to: peer.info,
                        result,
                    })
                    .await;
            });
        }
    }

    /// Broadcast entry point for a copied image: encode PNG (blocking CPU
    /// work) → check the cap → broadcast
    async fn broadcast_image(
        self: &Arc<Self>,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        timestamp_ms: u64,
    ) {
        let encoded =
            tokio::task::spawn_blocking(move || blob::encode_png_rgba(width, height, &rgba)).await;
        let Ok(Ok(png)) = encoded else {
            tracing::warn!("图像 PNG 编码失败, 不广播(本机历史不受影响)");
            return;
        };
        if png.len() as u64 > MAX_SYNC_IMAGE_BYTES {
            tracing::info!(
                bytes = png.len(),
                "图像编码后超过同步上限, 不广播(本机历史不受影响)"
            );
            return;
        }
        let meta = ImageOfferMeta {
            total_bytes: png.len() as u64,
            width,
            height,
        };
        let Ok(meta_json) = serde_json::to_string(&meta) else {
            return;
        };
        self.broadcast_blob(
            content_type::IMAGE,
            meta_json,
            Arc::new(Vec::new()),
            BlobSource::Memory(png.into()),
            timestamp_ms,
        )
        .await;
    }

    /// Broadcast entry point for copied files: vet each path (regular files
    /// only, plus the total cap) → broadcast
    ///
    /// If the set contains a directory or a special file, or exceeds the cap,
    /// none of it is synced — it is logged, and local history still records it
    /// as usual. A partial sync would hand the peer a batch with pieces
    /// missing, which is worse than plainly not sending.
    async fn broadcast_files(self: &Arc<Self>, paths: Vec<PathBuf>, timestamp_ms: u64) {
        if paths.is_empty() || paths.len() > MAX_SYNC_FILE_COUNT {
            tracing::info!(count = paths.len(), "文件数超出同步范围, 不广播");
            return;
        }
        let mut metas = Vec::with_capacity(paths.len());
        let mut total: u64 = 0;
        for path in &paths {
            // metadata follows symlinks before the type check: a link pointing
            // at a regular file is fine to sync
            let Ok(meta) = tokio::fs::metadata(path).await else {
                tracing::info!(path = %path.display(), "文件不可读, 整批不同步");
                return;
            };
            if !meta.is_file() {
                tracing::info!(path = %path.display(), "含目录/特殊文件, 整批不同步(v1 仅常规文件)");
                return;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            total += meta.len();
            metas.push(FileMeta {
                name,
                bytes: meta.len(),
            });
        }
        let limit = self.max_sync_file_bytes.load(Ordering::Relaxed);
        if total == 0 || total > limit {
            tracing::info!(total, limit, "文件总量超出同步上限, 不广播");
            return;
        }
        let offer = FilesOfferMeta {
            total_bytes: total,
            files: metas.clone(),
        };
        let Ok(meta_json) = serde_json::to_string(&offer) else {
            return;
        };
        self.broadcast_blob(
            content_type::FILES,
            meta_json,
            Arc::new(metas),
            BlobSource::Files(Arc::new(paths)),
            timestamp_ms,
        )
        .await;
    }
}

/// Sync engine handle: starts the background tasks and exposes the control
/// surface
pub struct SyncEngine {
    /// Shared state
    inner: Arc<Inner>,
    /// Background task handles (aborted on shutdown)
    tasks: Vec<JoinHandle<()>>,
}

impl SyncEngine {
    /// Start the engine: load the identity, bind the listener, start discovery
    /// and the pump tasks
    ///
    /// `clipboard_rx` is where clipboard change events come in — the
    /// production shell passes the receiver from
    /// [`crate::clipboard::spawn_watcher`], tests inject their own.
    pub async fn start(
        config: EngineConfig,
        clipboard_rx: mpsc::Receiver<ClipboardEvent>,
    ) -> Result<(Self, mpsc::Receiver<EngineEvent>), SyncError> {
        let identity = Arc::new(DeviceIdentity::load_or_create(&config.data_dir)?);
        let server_tls = Arc::new(tls::server_config(&identity)?);
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.tcp_port)).await?;
        let port = listener.local_addr()?.port();
        let (discovery, peer_rx) =
            DiscoveryService::start(&identity, port, config.discovery_port, config.passive).await?;
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAP);
        let inner = Arc::new(Inner {
            identity: Mutex::new(identity),
            data_dir: config.data_dir.clone(),
            server_tls,
            discovery,
            paired: Mutex::new(paired::PairedStore::load(&config.data_dir)),
            paired_io: Arc::new(tokio::sync::Mutex::new(())),
            events: events_tx,
            seq: AtomicU64::new(0),
            last_local_copy_ms: AtomicU64::new(0),
            echo: Mutex::new(VecDeque::new()),
            send_enabled: AtomicBool::new(config.sync_mode.sends()),
            recv_enabled: AtomicBool::new(config.sync_mode.receives()),
            sync_text: AtomicBool::new(config.sync_types.text),
            sync_images: AtomicBool::new(config.sync_types.images),
            sync_files: AtomicBool::new(config.sync_types.files),
            max_sync_file_bytes: AtomicU64::new(config.max_sync_file_bytes),
            pending_pairs: Mutex::new(HashMap::new()),
            pending_seq: AtomicU64::new(0),
            client_tls: Mutex::new(HashMap::new()),
            port,
        });
        let tasks = vec![
            tokio::spawn(net::accept_loop(Arc::clone(&inner), listener)),
            spawn_discovery_pump(Arc::clone(&inner), peer_rx),
            spawn_clipboard_pump(Arc::clone(&inner), clipboard_rx),
        ];
        Ok((Self { inner, tasks }, events_rx))
    }

    /// Local device info
    pub fn local_info(&self) -> PeerInfo {
        self.inner.current_identity().peer_info()
    }

    /// Update the display name live and re-announce at once (None = go back to
    /// following the hostname)
    ///
    /// The fingerprint and certificate are untouched: the device identity does
    /// not change. Only the display name in identity.json is persisted and the
    /// in-memory snapshot swapped; discovery re-announces through update_info,
    /// so peers never see the device go offline and come back.
    pub fn set_display_name(&self, name: Option<&str>) -> Result<(), SyncError> {
        crate::identity::persist_display_name(&self.inner.data_dir, name)?;
        let refreshed = Arc::new(DeviceIdentity::load_or_create(&self.inner.data_dir)?);
        let info = refreshed.peer_info();
        *self
            .inner
            .identity
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = refreshed;
        self.inner.discovery.update_info(&info);
        Ok(())
    }

    /// The port actually bound (whatever was assigned when configured as 0)
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// Initiate pairing with a peer; blocks until the remote user decides or
    /// it times out
    pub async fn pair(&self, fingerprint: &str) -> Result<(), SyncError> {
        let peer = self
            .inner
            .discovery
            .peer_by_fingerprint(fingerprint)
            .ok_or(SyncError::PeerUnreachable)?;
        let identity = self.inner.current_identity();
        let config = self.inner.client_tls_for(fingerprint)?;
        let remote = net::pair_transaction(&identity, config, &peer).await?;
        self.inner.add_paired(&remote).await;
        Ok(())
    }

    /// Feed back the user's decision on an inbound pairing request; answers
    /// [`EngineEvent::PairRequested`]
    pub fn respond_pair(&self, fingerprint: &str, accept: bool) {
        if let Some(pending) = self.inner.lock_pending().remove(fingerprint) {
            let _ = pending.tx.send(accept);
        }
    }

    /// Snapshot of the inbound pairing requests waiting on a UI decision
    ///
    /// The shell layer's startup fallback: the event pump is ready before the
    /// frontend is, so a PairRequested event arriving in that window has no
    /// listener and is dropped; the frontend pulls them from here once it
    /// mounts.
    pub fn pending_pair_requests(&self) -> Vec<PeerInfo> {
        self.inner
            .lock_pending()
            .values()
            .map(|p| p.peer.clone())
            .collect()
    }

    /// Unpair: takes effect locally at once — that is the security boundary —
    /// then notifies the peer on a best-effort basis
    pub async fn unpair(&self, fingerprint: &str) {
        let peer = self.inner.discovery.peer_by_fingerprint(fingerprint);
        self.inner.remove_paired(fingerprint).await;
        if let Some(peer) = peer {
            let identity = self.inner.current_identity();
            match self.inner.client_tls_for(fingerprint) {
                Ok(config) => {
                    if let Err(e) = net::unpair_transaction(&identity, config, &peer).await {
                        tracing::debug!("解除配对通知发送失败(对端下次同步时会被拒): {e}");
                    }
                }
                Err(e) => tracing::debug!("解除配对通知未发出(TLS 配置构建失败): {e}"),
            }
        }
    }

    /// Switch the sync direction policy; both directions take effect at once
    pub fn set_sync_mode(&self, mode: SyncMode) {
        self.inner
            .send_enabled
            .store(mode.sends(), Ordering::Relaxed);
        self.inner
            .recv_enabled
            .store(mode.receives(), Ordering::Relaxed);
    }

    /// Switch the sync type toggles; they apply to sending and receiving alike
    pub fn set_sync_types(&self, types: SyncTypes) {
        self.inner.sync_text.store(types.text, Ordering::Relaxed);
        self.inner
            .sync_images
            .store(types.images, Ordering::Relaxed);
        self.inner.sync_files.store(types.files, Ordering::Relaxed);
    }

    /// Update the cap on the total bytes of a file sync
    pub fn set_max_sync_file_bytes(&self, bytes: u64) {
        self.inner
            .max_sync_file_bytes
            .store(bytes, Ordering::Relaxed);
    }

    /// Cancel one echo registration; the shell layer calls this when writing
    /// the clipboard failed (see the [`EngineEvent::ApplyRemote`] contract)
    pub fn cancel_echo(&self, hash: &str) {
        if self.inner.take_echo(hash) {
            tracing::debug!("剪贴板写入失败, 已撤销对应的回声登记");
        }
    }

    /// Snapshot of the peers currently online
    pub fn peers(&self) -> Vec<Peer> {
        self.inner.discovery.peers()
    }

    /// The current pairing list
    pub fn paired_list(&self) -> Vec<PairedPeer> {
        self.inner.lock_paired().list()
    }

    /// Graceful shutdown: send the discovery goodbye, then stop every
    /// background task
    pub async fn shutdown(&self) {
        self.inner.discovery.shutdown().await;
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Discovery event pump: forwards PeerEvent through as EngineEvent
fn spawn_discovery_pump(
    inner: Arc<Inner>,
    mut peer_rx: mpsc::Receiver<PeerEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = peer_rx.recv().await {
            let mapped = match event {
                PeerEvent::Up(peer) => EngineEvent::PeerUp(peer),
                PeerEvent::Down(fp) => EngineEvent::PeerDown(fp),
            };
            inner.emit(mapped).await;
        }
    })
}

/// What the pump decided to send out; the checks run before any clone (see the
/// comments inside the pump)
enum Outbound {
    /// Nothing goes out for this event
    None,
    /// Broadcast text
    Text(String),
    /// Broadcast an image (width/height/RGBA)
    Image(usize, usize, Vec<u8>),
    /// Broadcast files
    Files(Vec<PathBuf>),
}

/// Clipboard event pump: filter echoes → update the LWW baseline → emit the
/// history/UI event → broadcast by type
fn spawn_clipboard_pump(
    inner: Arc<Inner>,
    mut clipboard_rx: mpsc::Receiver<ClipboardEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = clipboard_rx.recv().await {
            // Echo: a remote write coming back around through the system
            // clipboard, swallowed once. The LWW baseline is not updated —
            // this was not the local user copying anything
            if inner.take_echo(&event.hash) {
                tracing::debug!("回声抑制: 跳过远端写入的绕回事件");
                continue;
            }
            // fetch_max rather than a plain store: a backwards clock jump (an
            // NTP correction, or the user changing the time) makes a new
            // event's timestamp smaller than an older one's, and storing it
            // would drag the LWW baseline backwards — after which an older
            // piece of remote content could overwrite what was just copied
            // locally
            inner
                .last_local_copy_ms
                .fetch_max(event.timestamp_ms, Ordering::Relaxed);
            // Each of the three content kinds has its own sync path; the
            // history pipeline consumes LocalCopied for all of them. The
            // direction, type and cap checks run before the clone, so nothing
            // large is copied for content that is going to be dropped anyway
            let send = inner.send_enabled.load(Ordering::Relaxed);
            let outbound = match &event.content {
                ClipboardContent::Text(text) if send && inner.sync_text.load(Ordering::Relaxed) => {
                    if text.len() > MAX_SYNC_TEXT_BYTES {
                        tracing::info!(
                            bytes = text.len(),
                            "文本超过同步上限, 不广播(本机历史不受影响)"
                        );
                        Outbound::None
                    } else {
                        Outbound::Text(text.clone())
                    }
                }
                ClipboardContent::Image {
                    width,
                    height,
                    rgba,
                } if send && inner.sync_images.load(Ordering::Relaxed) => {
                    // This one rgba clone is unavoidable: LocalCopied hands
                    // the original to history, and the broadcast needs its own
                    // copy to encode (encoding runs inside spawn_blocking)
                    Outbound::Image(*width, *height, rgba.clone())
                }
                ClipboardContent::Files(paths)
                    if send && inner.sync_files.load(Ordering::Relaxed) =>
                {
                    Outbound::Files(paths.clone())
                }
                _ => Outbound::None,
            };
            inner
                .emit(EngineEvent::LocalCopied {
                    content: event.content,
                    hash: event.hash,
                    timestamp_ms: event.timestamp_ms,
                })
                .await;
            match outbound {
                Outbound::None => {}
                // The frame cap check (JSON escaping inflates text by up to 6x
                // in the worst case) is folded into broadcast_text's single
                // encode: it refuses at encode time and logs
                Outbound::Text(text) => inner.broadcast_text(text, event.timestamp_ms).await,
                Outbound::Image(w, h, rgba) => {
                    inner.broadcast_image(w, h, rgba, event.timestamp_ms).await
                }
                Outbound::Files(paths) => inner.broadcast_files(paths, event.timestamp_ms).await,
            }
        }
    })
}

#[cfg(test)]
mod tests;
