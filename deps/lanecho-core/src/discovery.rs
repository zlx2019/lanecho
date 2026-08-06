//! Discovery layer: lets lanecho nodes on the LAN see each other.
//!
//! Two channels:
//! - Primary: mDNS/DNS-SD registration and browsing of the
//!   `_lanecho._tcp.local.` service
//! - Fallback: periodic UDP multicast announce (some enterprise routers
//!   block mDNS)
//!
//! Either channel alone is enough to work; only a failure of both is an
//! error.
//! Node lifecycle: 5s heartbeat → 15s timeout goes offline → goodbye packet
//! on exit.
//! Discovery packets carry only small fields (name/platform/port/fingerprint);
//! there is no avatar mechanism.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::identity::DeviceIdentity;
use crate::protocol::PeerInfo;
use crate::tls;

use crate::config::{
    EVENT_CHANNEL_CAP, HEARTBEAT_INTERVAL, PEER_PROBE_INTERVAL, PEER_PROBE_TIMEOUT, PEER_TIMEOUT,
};

/// mDNS service type
pub const MDNS_SERVICE_TYPE: &str = "_lanecho._tcp.local.";
/// UDP multicast group (the 224.0.0.0/24 range has the best router
/// compatibility)
const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 169);

/// Discovery layer error
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// Both the mDNS and the UDP multicast channel failed to initialize
    #[error("发现服务不可用: mDNS 与 UDP 组播均初始化失败")]
    AllChannelsFailed,
    /// UDP socket operation failed
    #[error("UDP 组播通道错误: {0}")]
    Io(#[from] std::io::Error),
    /// mDNS daemon error
    #[error("mDNS 通道错误: {0}")]
    Mdns(#[from] mdns_sd::Error),
}

/// An online node on the LAN
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// Device info (ID/name/fingerprint/platform)
    pub info: PeerInfo,
    /// Candidate addresses (more than one with multiple NICs; non-loopback
    /// IPv4 comes first, and they are tried in order when connecting)
    pub addrs: Vec<IpAddr>,
    /// TCP port of the control/data channel
    pub port: u16,
}

/// Node up/down event
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// Node came online, or its info changed
    Up(Peer),
    /// Node went offline (payload is the certificate fingerprint)
    Down(String),
}

/// UDP multicast packet kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AnnounceKind {
    /// Periodic broadcast: I am online
    Announce,
    /// Unicast reply to an announce, so a new node sees the existing ones
    /// right away
    Response,
    /// Graceful go offline
    Goodbye,
}

/// UDP multicast packet (the IP is taken from the UDP source address rather
/// than carried in the body)
#[derive(Debug, Serialize, Deserialize)]
struct AnnouncePacket {
    /// Packet kind
    kind: AnnounceKind,
    /// Device info
    info: PeerInfo,
    /// TCP listening port
    tcp_port: u16,
}

/// Serialize a UDP multicast packet; returns empty bytes on failure (every
/// field is a simple type, so that cannot happen in practice)
fn encode_packet(kind: AnnounceKind, info: &PeerInfo, tcp_port: u16) -> Vec<u8> {
    serde_json::to_vec(&AnnouncePacket {
        kind,
        info: info.clone(),
        tcp_port,
    })
    .unwrap_or_default()
}

/// Pre-serialized packet set for the UDP channel (replaced as a whole when
/// the identity is updated, and read by each task before sending)
struct UdpPackets {
    /// Periodic broadcast
    announce: Vec<u8>,
    /// Unicast reply to an announce
    response: Vec<u8>,
    /// Graceful go offline
    goodbye: Vec<u8>,
}

impl UdpPackets {
    /// Encode the whole set from the current identity; passive (incognito)
    /// mode only receives, so every packet is left empty
    fn encode(info: &PeerInfo, tcp_port: u16, passive: bool) -> Self {
        if passive {
            return Self {
                announce: Vec::new(),
                response: Vec::new(),
                goodbye: Vec::new(),
            };
        }
        Self {
            announce: encode_packet(AnnounceKind::Announce, info, tcp_port),
            response: encode_packet(AnnounceKind::Response, info, tcp_port),
            goodbye: encode_packet(AnnounceKind::Goodbye, info, tcp_port),
        }
    }
}

/// Shared handle to the packet set
type SharedPackets = Arc<std::sync::RwLock<UdpPackets>>;

/// Read the packet set (a poisoned lock simply recovers the inner data)
fn read_packets(packets: &SharedPackets) -> std::sync::RwLockReadGuard<'_, UdpPackets> {
    packets.read().unwrap_or_else(PoisonError::into_inner)
}

/// Node registry: merges the mDNS and UDP sources, tracks online state and
/// dispatches events
struct Registry {
    /// Our own fingerprint (filters out self-sightings)
    self_fingerprint: String,
    /// Online nodes, keyed by certificate fingerprint
    peers: Mutex<HashMap<String, PeerState>>,
    /// Event sender (drops when full; consumers can fall back to a snapshot)
    events: mpsc::Sender<PeerEvent>,
}

/// Which channel a node's info came from
#[derive(Clone, Copy, PartialEq, Eq)]
enum PeerSource {
    /// mDNS browsing: event driven, no repeated Resolved events while the
    /// service stays up
    Mdns,
    /// UDP multicast: heartbeat driven, refreshed every heartbeat period
    Udp,
}

/// Node state inside the registry
///
/// Liveness is tracked per source channel: mDNS can only be declared dead by
/// ServiceRemoved (goodbye or TTL expiry), never by elapsed time — it has no
/// periodic heartbeat to refresh; UDP is dead once no heartbeat arrives
/// within the timeout window. A node goes offline only when both channels are
/// dead, otherwise it would be kicked wrongly after 15s wherever multicast is
/// blocked by the network and only mDNS gets through.
struct PeerState {
    /// Node info
    peer: Peer,
    /// Time of the last UDP heartbeat (None if never seen over UDP)
    last_udp: Option<Instant>,
    /// mDNS channel is alive (set by ServiceResolved, cleared by
    /// ServiceRemoved)
    mdns_alive: bool,
    /// When the last liveness probe was started (used for throttling; a node
    /// coming online counts as just probed)
    last_probe: Option<Instant>,
}

impl Registry {
    /// Take the lock; a poisoned lock simply recovers the inner data (the map
    /// holds no cross-thread invariant)
    fn lock_peers(&self) -> MutexGuard<'_, HashMap<String, PeerState>> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Insert or refresh a node; an Up event is emitted only when the info
    /// changed, a heartbeat merely refreshes the timestamp
    ///
    /// Address merging: mDNS (many addresses) and UDP (a single source
    /// address) complement each other; the union is deduped and then
    /// normalized (IPv4 first, stable sort so alternating channels do not
    /// make the order jitter). An update that normalizes to no usable address
    /// is dropped, waiting for a later event that carries one. Stale
    /// addresses are not pruned one by one; the list is rebuilt after the
    /// node as a whole times out and goes offline.
    fn upsert(&self, mut peer: Peer, source: PeerSource) {
        if peer.info.fingerprint == self.self_fingerprint {
            return;
        }
        let mut peers = self.lock_peers();
        let fingerprint = peer.info.fingerprint.clone();
        // Inherit the other channel's liveness flag: a new source only adds
        // liveness, it never clears the other one
        let (changed, mut last_udp, mut mdns_alive, last_probe) = match peers.get(&fingerprint) {
            Some(state) => {
                let mut merged = state.peer.addrs.clone();
                for addr in &peer.addrs {
                    if !merged.contains(addr) {
                        merged.push(*addr);
                    }
                }
                peer.addrs = normalize_addrs(merged);
                (
                    state.peer != peer,
                    state.last_udp,
                    state.mdns_alive,
                    state.last_probe,
                )
            }
            None => {
                peer.addrs = normalize_addrs(peer.addrs);
                // Coming online is itself proof of life, so count it as just
                // probed: the first probe waits out a full interval
                (true, None, false, Some(Instant::now()))
            }
        };
        if peer.addrs.is_empty() {
            return;
        }
        match source {
            PeerSource::Udp => last_udp = Some(Instant::now()),
            PeerSource::Mdns => mdns_alive = true,
        }
        peers.insert(
            fingerprint,
            PeerState {
                peer: peer.clone(),
                last_udp,
                mdns_alive,
                last_probe,
            },
        );
        drop(peers);
        if changed {
            self.emit(PeerEvent::Up(peer));
        }
    }

    /// Remove a node by fingerprint and emit a Down event
    fn remove(&self, fingerprint: &str) {
        let existed = self.lock_peers().remove(fingerprint).is_some();
        if existed {
            self.emit(PeerEvent::Down(fingerprint.to_string()));
        }
    }

    /// The mDNS service disappeared (goodbye or TTL expiry): clear the mDNS
    /// liveness flag
    ///
    /// The node is kept while its UDP heartbeat is still fresh (degrading to
    /// one channel must not flicker it offline), otherwise it is removed at
    /// once. ServiceRemoved only gives the instance name, i.e. the device ID.
    fn mdns_removed(&self, device_id: &str, udp_timeout: Duration) {
        let mut peers = self.lock_peers();
        let Some((fp, state)) = peers
            .iter_mut()
            .find(|(_, s)| s.peer.info.device_id == device_id)
        else {
            return;
        };
        state.mdns_alive = false;
        let udp_dead = state.last_udp.is_none_or(|t| t.elapsed() > udp_timeout);
        let fp = fp.clone();
        drop(peers);
        if udp_dead {
            self.remove(&fp);
        }
    }

    /// Drop dead nodes and return the suspicious ones that need a liveness
    /// probe
    ///
    /// - Cleanup: mDNS not alive and the UDP heartbeat timed out (or never
    ///   arrived) → removed outright; a node alive over mDNS is never kicked
    ///   on elapsed time — it goes offline through ServiceRemoved
    /// - Probing: a node that is "mDNS-only alive with UDP silent" cannot be
    ///   declared dead by time (mDNS has no periodic heartbeat, and a crashed
    ///   node would wait out an SRV TTL of about 2min), so it is handed to
    ///   the caller for a connection probe. The probe timestamp is stamped
    ///   here (throttled by [`PEER_PROBE_INTERVAL`]), so being returned means
    ///   "due for a probe"
    fn sweep(&self, timeout: Duration) -> Vec<Peer> {
        let now = Instant::now();
        let mut probes = Vec::new();
        let expired: Vec<String> = {
            let mut peers = self.lock_peers();
            for state in peers.values_mut() {
                let udp_silent = state.last_udp.is_none_or(|t| t.elapsed() > timeout);
                let probe_due = state
                    .last_probe
                    .is_none_or(|t| t.elapsed() > PEER_PROBE_INTERVAL);
                if state.mdns_alive && udp_silent && probe_due {
                    state.last_probe = Some(now);
                    probes.push(state.peer.clone());
                }
            }
            peers
                .iter()
                .filter(|(_, s)| !s.mdns_alive && s.last_udp.is_none_or(|t| t.elapsed() > timeout))
                .map(|(fp, _)| fp.clone())
                .collect()
        };
        for fp in expired {
            tracing::debug!(fingerprint = %fp, "节点心跳超时, 判定下线");
            self.remove(&fp);
        }
        probes
    }

    /// Whether the node's UDP heartbeat is still inside the window (fresher
    /// evidence overrides the death decision of a failed probe)
    fn udp_fresh(&self, fingerprint: &str, timeout: Duration) -> bool {
        self.lock_peers()
            .get(fingerprint)
            .is_some_and(|s| s.last_udp.is_some_and(|t| t.elapsed() <= timeout))
    }

    /// Apply a failed probe directly: clear the mDNS liveness flag and remove
    /// the node
    ///
    /// Only used as a fallback when the mDNS daemon is unavailable (no cache
    /// verify to go through); the regular path is [`probe_peer`] — there the
    /// death decision is left to the ServiceRemoved that verify triggers, so
    /// the node table and the daemon cache are cleaned together.
    /// During the probe (a 2s window) the node may have just come back over
    /// UDP (a network blip healing, say); while the heartbeat is fresh the
    /// newer evidence wins and the node is kept — a probe can only refute
    /// liveness that rests on mDNS alone.
    fn probe_failed(&self, fingerprint: &str, udp_timeout: Duration) {
        let mut peers = self.lock_peers();
        let Some(state) = peers.get_mut(fingerprint) else {
            return;
        };
        if state.last_udp.is_some_and(|t| t.elapsed() <= udp_timeout) {
            return;
        }
        state.mdns_alive = false;
        drop(peers);
        tracing::info!(fingerprint = %fingerprint, "TCP 探活失败, 判定节点已崩溃下线");
        self.remove(fingerprint);
    }

    /// Snapshot of the currently online nodes
    fn snapshot(&self) -> Vec<Peer> {
        self.lock_peers().values().map(|s| s.peer.clone()).collect()
    }

    /// Emit an event; dropped when the channel is full (consumers can correct
    /// themselves from a snapshot at any time)
    fn emit(&self, event: PeerEvent) {
        if let Err(e) = self.events.try_send(event) {
            tracing::debug!("节点事件通道已满, 丢弃事件: {e}");
        }
    }
}

/// Mutable state on the broadcast side (incognito toggling and identity
/// updates share it; a single lock keeps them consistent)
struct BroadcastState {
    /// Identity currently broadcast (used to re-encode the packet set when
    /// leaving incognito)
    info: PeerInfo,
    /// Incognito mode: receive only, never send
    passive: bool,
    /// Full name of our mDNS service (Some while registered, used to
    /// unregister)
    mdns_fullname: Option<String>,
}

/// Discovery service: registers this device, listens to the network, and
/// reports node changes over an event channel
pub struct DiscoveryService {
    /// Node registry
    registry: Arc<Registry>,
    /// mDNS daemon (None if it failed to initialize, degrading to UDP only)
    mdns: Option<mdns_sd::ServiceDaemon>,
    /// UDP multicast socket (None if it failed to initialize, degrading to
    /// mDNS only)
    udp: Option<Arc<UdpSocket>>,
    /// UDP destination (multicast group + port)
    udp_target: (Ipv4Addr, u16),
    /// UDP packet set (replaced as a whole when the identity is updated)
    packets: SharedPackets,
    /// TCP port we broadcast (used to re-register mDNS; fixed after startup)
    tcp_port: u16,
    /// Broadcast identity / incognito switch / mDNS registered name (mutable
    /// at runtime)
    state: Mutex<BroadcastState>,
    /// Background task handles (aborted on shutdown)
    tasks: Vec<JoinHandle<()>>,
}

impl DiscoveryService {
    /// Start the discovery service: register this device, begin listening,
    /// and return the service handle plus the node event stream
    ///
    /// `identity` supplies both the broadcast identity (peer_info) and the
    /// TLS credentials for liveness probes — a suspicious node counts as
    /// alive only once its certificate fingerprint checks out, see
    /// `probe_peer` (a private item, hence not a doc link).
    /// With `passive` set to true the service only listens and never
    /// broadcasts (short-lived scan/send scenarios: no mDNS registration, no
    /// announce/response/goodbye), i.e. "incognito" mode.
    pub async fn start(
        identity: &DeviceIdentity,
        tcp_port: u16,
        discovery_port: u16,
        passive: bool,
    ) -> Result<(Self, mpsc::Receiver<PeerEvent>), DiscoveryError> {
        let info = identity.peer_info();
        // TLS config for probes: accept any certificate — the liveness
        // decision does not rest on a pin inside the handshake but on an
        // explicit comparison against the node's fingerprint afterwards (each
        // node expects a different fingerprint, so one pinned config cannot
        // be shared). A build failure (broken certificate) does not block
        // discovery: probing degrades to going straight to mDNS cache
        // verification, so the death decision chain stays intact and only
        // loses its fast TCP path
        let probe_tls = match tls::client_config(identity, None) {
            Ok(config) => Some(Arc::new(config)),
            Err(e) => {
                tracing::warn!("探活 TLS 配置构建失败, 退化为纯 mDNS 缓存验证: {e}");
                None
            }
        };
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAP);
        let registry = Arc::new(Registry {
            self_fingerprint: info.fingerprint.clone(),
            peers: Mutex::new(HashMap::new()),
            events: events_tx,
        });
        let mut tasks = Vec::new();

        // Channel one: mDNS registration + browsing
        let (mdns, mdns_fullname) =
            match start_mdns(&info, tcp_port, passive, &registry, &mut tasks) {
                Ok(pair) => (Some(pair.0), pair.1),
                Err(e) => {
                    tracing::warn!("mDNS 初始化失败, 降级为纯 UDP 组播: {e}");
                    (None, None)
                }
            };

        // Channel two: UDP multicast announce/response (the packet set is
        // shared, and replaced as a whole when the identity is updated)
        let packets: SharedPackets = Arc::new(std::sync::RwLock::new(UdpPackets::encode(
            &info, tcp_port, passive,
        )));
        let udp_target = (MULTICAST_GROUP, discovery_port);
        let udp = match start_udp(
            &info,
            discovery_port,
            Arc::clone(&packets),
            &registry,
            &mut tasks,
        )
        .await
        {
            Ok(socket) => Some(socket),
            Err(e) => {
                // Usually another instance on the same machine holds the
                // port; mDNS still lets them see each other
                tracing::warn!("UDP 组播初始化失败, 降级为纯 mDNS: {e}");
                None
            }
        };

        if mdns.is_none() && udp.is_none() {
            return Err(DiscoveryError::AllChannelsFailed);
        }

        // Timeout sweep + crash probing task (probes run concurrently so they
        // never stall the sweep cadence); the daemon is cloned for the probe
        // path to run cache verification (internally a command channel
        // handle, so cloning is cheap)
        let sweeper = Arc::clone(&registry);
        let sweeper_mdns = mdns.clone();
        tasks.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(HEARTBEAT_INTERVAL);
            loop {
                tick.tick().await;
                for peer in sweeper.sweep(PEER_TIMEOUT) {
                    tokio::spawn(probe_peer(
                        Arc::clone(&sweeper),
                        sweeper_mdns.clone(),
                        probe_tls.clone(),
                        peer,
                    ));
                }
            }
        }));

        // Sleep/wake self-healing task (only when the UDP channel is up)
        if let Some(socket) = &udp {
            tasks.push(tokio::spawn(sleep_watchdog(
                Arc::clone(socket),
                udp_target,
                Arc::clone(&packets),
            )));
        }

        Ok((
            Self {
                registry,
                mdns,
                udp,
                udp_target,
                packets,
                tcp_port,
                state: Mutex::new(BroadcastState {
                    info,
                    passive,
                    mdns_fullname,
                }),
                tasks,
            },
            events_rx,
        ))
    }

    /// Toggle incognito mode live (takes effect at once without interrupting
    /// discovery; a no-op when it already matches the current state)
    ///
    /// Turning it on: send goodbye first so peers drop us immediately, then
    /// empty the UDP packet set (silencing the heartbeat) and unregister the
    /// mDNS service (peers drop us through ServiceRemoved).
    /// Turning it off: re-encode the packet set, register mDNS again, and
    /// announce right away so peers see us sooner.
    /// Like update_info, this may be called from a thread with no tokio
    /// runtime (the IPC thread of a synchronous Tauri command), so it uses
    /// only synchronous, non-blocking calls throughout.
    pub fn set_passive(&self, passive: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.passive == passive {
            return;
        }
        state.passive = passive;
        if passive {
            // Grab the goodbye packet before emptying the set; the other
            // order leaves nothing to send. UDP is unreliable, so send it
            // twice for a better chance of arrival; a failure is harmless
            // (peers fall back to the heartbeat timeout)
            let goodbye = read_packets(&self.packets).goodbye.clone();
            if let Some(udp) = &self.udp
                && !goodbye.is_empty()
            {
                for _ in 0..2 {
                    let _ = udp.try_send_to(&goodbye, self.udp_target.into());
                }
            }
            *self.packets.write().unwrap_or_else(PoisonError::into_inner) =
                UdpPackets::encode(&state.info, self.tcp_port, true);
            if let Some(daemon) = &self.mdns
                && let Some(fullname) = state.mdns_fullname.take()
                && let Err(e) = daemon.unregister(&fullname)
            {
                tracing::warn!("mDNS 注销失败(对端将等 TTL 过期才下线): {e}");
            }
            tracing::info!("已进入隐身模式(只收不发)");
        } else {
            *self.packets.write().unwrap_or_else(PoisonError::into_inner) =
                UdpPackets::encode(&state.info, self.tcp_port, false);
            if let Some(daemon) = &self.mdns {
                match build_mdns_service(&state.info, self.tcp_port) {
                    Ok(service) => {
                        let fullname = service.get_fullname().to_string();
                        match daemon.register(service) {
                            Ok(()) => state.mdns_fullname = Some(fullname),
                            Err(e) => tracing::warn!("mDNS 重新注册失败: {e}"),
                        }
                    }
                    Err(e) => tracing::warn!("mDNS 服务信息构造失败: {e}"),
                }
            }
            // Announce once immediately so peers see us right away
            // (otherwise they wait for the next heartbeat period)
            if let Some(udp) = &self.udp {
                let announce = read_packets(&self.packets).announce.clone();
                if !announce.is_empty()
                    && let Err(e) = udp.try_send_to(&announce, self.udp_target.into())
                {
                    tracing::debug!("退出隐身的即时 announce 发送失败(心跳会补发): {e}");
                }
            }
            tracing::info!("已退出隐身模式, 恢复广播");
        }
    }

    /// Update the broadcast identity live (a nickname change takes effect at
    /// once without interrupting discovery)
    ///
    /// Fingerprint and port are the root of the identity and cannot change;
    /// only display fields (name) may be updated:
    /// - UDP: the whole packet set is re-encoded so heartbeat/response/
    ///   goodbye carry the new content immediately, plus one extra announce
    ///   so peers see it sooner (otherwise they wait for the next heartbeat)
    /// - mDNS: register again under the same name — the daemon treats that as
    ///   an overwrite and broadcasts the new TXT record at once, so peers
    ///   never see us go offline and come back
    pub fn update_info(&self, info: &PeerInfo) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.info = info.clone();
        *self.packets.write().unwrap_or_else(PoisonError::into_inner) =
            UdpPackets::encode(info, self.tcp_port, state.passive);
        if state.passive {
            return;
        }
        // The broadcast actions stay inside the state lock, mutually
        // exclusive with set_passive: outside it, "read not-incognito → a
        // concurrent switch into incognito finishes unregistering → this code
        // registers anyway" would put an incognito machine back on the air
        // (mdns-sd register only posts to a command channel, it never blocks)
        if let Some(udp) = &self.udp {
            // Must use the synchronous API: this method may be called from a
            // thread with no tokio runtime (the IPC thread of a synchronous
            // Tauri command, say), where tokio::spawn panics outright. A
            // single UDP try_send_to completes instantly, and the 5s
            // heartbeat covers the occasional failure.
            let announce = read_packets(&self.packets).announce.clone();
            if !announce.is_empty()
                && let Err(e) = udp.try_send_to(&announce, self.udp_target.into())
            {
                tracing::debug!("身份更新的即时 announce 发送失败(心跳会补发): {e}");
            }
        }
        if let Some(daemon) = &self.mdns {
            match build_mdns_service(info, self.tcp_port) {
                Ok(service) => {
                    let fullname = service.get_fullname().to_string();
                    match daemon.register(service) {
                        // Same-name register overwrites; remember the latest
                        // fullname so a later unregister matches
                        Ok(()) => state.mdns_fullname = Some(fullname),
                        Err(e) => tracing::warn!("mDNS 身份更新失败: {e}"),
                    }
                }
                Err(e) => tracing::warn!("mDNS 服务信息构造失败: {e}"),
            }
        }
    }

    /// Snapshot of the currently online nodes (a fallback query beside the
    /// event stream)
    pub fn peers(&self) -> Vec<Peer> {
        self.registry.snapshot()
    }

    /// Look up a single online node by certificate fingerprint (clones only
    /// the hit instead of snapshotting the whole table)
    pub fn peer_by_fingerprint(&self, fingerprint: &str) -> Option<Peer> {
        self.registry
            .lock_peers()
            .get(fingerprint)
            .map(|s| s.peer.clone())
    }

    /// Graceful shutdown: send goodbye, unregister mDNS, stop the background
    /// tasks (idempotent, safe to call repeatedly)
    pub async fn shutdown(&self) {
        if let Some(udp) = &self.udp {
            // UDP is unreliable, so send goodbye twice for a better chance of
            // arrival (in passive mode the packet is empty and nothing goes
            // out)
            let goodbye = read_packets(&self.packets).goodbye.clone();
            if !goodbye.is_empty() {
                for _ in 0..2 {
                    let _ = udp.send_to(&goodbye, self.udp_target).await;
                }
            }
        }
        if let Some(mdns) = &self.mdns {
            // While incognito the fullname is None (already unregistered),
            // but the daemon's browsing still has to be stopped
            let fullname = self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .mdns_fullname
                .take();
            if let Some(fullname) = fullname {
                let _ = mdns.unregister(&fullname);
            }
            let _ = mdns.shutdown();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Probe a suspicious "mDNS-only alive" node: TLS handshake against each
/// candidate address in turn and compare the certificate fingerprint
///
/// **A liveness decision must identify the peer; a bare connect is not proof
/// of life.** The address list only grows (see upsert), so a stale address may
/// already have been reassigned by DHCP to a different device that also
/// listens on this port — the handshake still succeeds, and the disconnected
/// node therefore stays **online forever** in everyone else's list; meanwhile
/// the sync layer, which pins the fingerprint, fails every time, producing the
/// split of "online in the list yet sync never gets through". So a node counts
/// as alive only when the handshake succeeds and the peer certificate
/// fingerprint matches the node's; reaching somebody else (fingerprint
/// mismatch) does not count, and the next address is tried — the real node may
/// still be among the remaining ones. The listening port of a crashed process
/// is reclaimed by the OS at once (connect gets an instant RST); unplugged
/// cables, power loss and other silent cases are covered by
/// [`PEER_PROBE_TIMEOUT`] (which spans both TCP and TLS).
///
/// When the real node is not found the node is **not removed directly**; mDNS
/// cache verification is triggered instead (RFC 6762 §10.4): the daemon
/// queries that instance, and 10s without an answer flushes the cache and
/// emits ServiceRemoved, so the node is removed through the existing
/// mdns_removed path — node table and daemon cache are cleaned in one step,
/// and any later reconnect by the peer is a fresh discovery (an Up event is
/// guaranteed). Removing directly breaks that: the table is cleared ahead of
/// the cache, and when the peer reconnects before the cache expires (TTL of
/// about 2min) an unchanged record only refreshes the TTL without emitting any
/// event, leaving us permanently blind to it. If the probe was wrong and the
/// peer is in fact alive, verify gets an answer, cache and node are kept, and
/// the next probe round connects successfully and heals itself.
async fn probe_peer(
    registry: Arc<Registry>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    peer: Peer,
) {
    if let Some(config) = &tls_config {
        for addr in &peer.addrs {
            let probed =
                tokio::time::timeout(PEER_PROBE_TIMEOUT, probe_identity(config, *addr, peer.port))
                    .await;
            match probed {
                // Fingerprint matches: the peer is who it claims to be, so it
                // is alive (the connection closes as the stream drops)
                Ok(Some(fp)) if fp == peer.info.fingerprint => return,
                Ok(Some(_)) => {
                    tracing::debug!(
                        %addr, name = %peer.info.name,
                        "探活连通但对端指纹不符(陈旧地址指向他机), 不算活证"
                    );
                }
                // Cannot connect / handshake failed / timed out: move on to
                // the next candidate address
                _ => {}
            }
        }
    }
    // The UDP heartbeat came back within the probe window: the newer evidence
    // wins, leave the node alone
    if registry.udp_fresh(&peer.info.fingerprint, PEER_TIMEOUT) {
        return;
    }
    match &mdns {
        Some(daemon) => {
            let fullname = format!("{}.{MDNS_SERVICE_TYPE}", peer.info.device_id);
            tracing::info!(name = %peer.info.name, "TCP 探活失败, 触发 mDNS 缓存验证");
            if let Err(e) = daemon.verify(fullname, mdns_sd::VERIFY_TIMEOUT_DEFAULT) {
                tracing::warn!("mDNS 缓存验证发起失败, 退回直接移除: {e}");
                registry.probe_failed(&peer.info.fingerprint, PEER_TIMEOUT);
            }
        }
        // With the mDNS channel unavailable the table holds no mdns_alive
        // node, so this arm is purely defensive
        None => registry.probe_failed(&peer.info.fingerprint, PEER_TIMEOUT),
    }
}

/// Complete a TLS handshake against one address and return the peer's
/// certificate fingerprint (None if the connect or the handshake fails)
///
/// The connection closes as soon as the handshake completes (the stream drops
/// when the return value leaves scope) and no application data is sent — the
/// accepting side reads EOF and finishes normally, leaving no half-open
/// connection.
async fn probe_identity(
    config: &Arc<rustls::ClientConfig>,
    addr: IpAddr,
    port: u16,
) -> Option<String> {
    let tcp = tokio::net::TcpStream::connect((addr, port)).await.ok()?;
    let connector = tokio_rustls::TlsConnector::from(Arc::clone(config));
    // ServerName is only required by the API (verification is the fingerprint
    // comparison), and a constant input cannot fail
    let name = rustls_pki_types::ServerName::try_from("lanecho").ok()?;
    let stream = connector.connect(name, tcp).await.ok()?;
    tls::peer_fingerprint(stream.get_ref().1.peer_certificates())
}

/// Sleep/wake self-healing: once the system resumes from sleep, rejoin the
/// multicast group and announce immediately
///
/// System sleep silently loses the IGMP multicast membership — after waking we
/// receive nobody's announce and nobody discovers us, so discovery is dead
/// without a single error.
/// Detection: the monotonic clock (Instant) stalls while asleep whereas the
/// wall clock (SystemTime) keeps running; the difference between how far each
/// advanced is the stall duration, and exceeding the threshold means we slept.
/// (The mDNS daemon watches network interfaces itself, so its wake recovery is
/// handled internally; on Windows the monotonic clock counts sleep time, and
/// when detection misses, the heartbeat timeout converges anyway.)
async fn sleep_watchdog(udp: Arc<UdpSocket>, target: (Ipv4Addr, u16), packets: SharedPackets) {
    /// Detection period
    const TICK: Duration = Duration::from_secs(30);
    /// A stall longer than this counts as a resume from sleep (NTP
    /// corrections are far smaller, so there are no false positives)
    const STALL_JUMP: Duration = Duration::from_secs(60);
    let mut wall = std::time::SystemTime::now();
    let mut mono = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let wall_gap = std::time::SystemTime::now()
            .duration_since(wall)
            .unwrap_or_default();
        let mono_gap = mono.elapsed();
        wall = std::time::SystemTime::now();
        mono = Instant::now();
        let stalled = wall_gap.saturating_sub(mono_gap);
        if stalled < STALL_JUMP {
            continue;
        }
        tracing::info!(
            stalled_secs = stalled.as_secs(),
            "检测到系统睡眠恢复, 重建组播成员关系"
        );
        // Leave before joining: joining again while the socket still holds
        // the old membership errors out
        let _ = udp.leave_multicast_v4(target.0, Ipv4Addr::UNSPECIFIED);
        if let Err(e) = udp.join_multicast_v4(target.0, Ipv4Addr::UNSPECIFIED) {
            tracing::warn!("重新加入组播组失败(等待下轮重试): {e}");
            continue;
        }
        // Announce once immediately so peers see us right away (in passive
        // mode the packet is empty and nothing goes out)
        let announce = read_packets(&packets).announce.clone();
        if !announce.is_empty() {
            let _ = udp.send_to(&announce, target).await;
        }
    }
}

/// Build this device's mDNS service info (shared by the first registration
/// and by identity updates)
///
/// The instance uses device_id so it stays unique and does not change with the
/// nickname (the fullname stays stable); the host reuses that name to avoid
/// clashing with the real hostname record.
fn build_mdns_service(
    info: &PeerInfo,
    tcp_port: u16,
) -> Result<mdns_sd::ServiceInfo, mdns_sd::Error> {
    let mut props: HashMap<String, String> = [
        ("id".to_string(), info.device_id.clone()),
        ("name".to_string(), info.name.clone()),
        ("fp".to_string(), info.fingerprint.clone()),
        ("platform".to_string(), info.platform.clone()),
    ]
    .into();
    // OS version description (optional)
    if let Some(osv) = &info.os_version {
        props.insert("osv".to_string(), osv.clone());
    }
    Ok(mdns_sd::ServiceInfo::new(
        MDNS_SERVICE_TYPE,
        &info.device_id,
        &format!("{}.local.", info.device_id),
        "",
        tcp_port,
        props,
    )?
    .enable_addr_auto())
}

/// Initialize mDNS: register our service (skipped when passive) and start the
/// browsing task
fn start_mdns(
    info: &PeerInfo,
    tcp_port: u16,
    passive: bool,
    registry: &Arc<Registry>,
    tasks: &mut Vec<JoinHandle<()>>,
) -> Result<(mdns_sd::ServiceDaemon, Option<String>), DiscoveryError> {
    let daemon = mdns_sd::ServiceDaemon::new()?;

    let fullname = if passive {
        None
    } else {
        let service = build_mdns_service(info, tcp_port)?;
        let name = service.get_fullname().to_string();
        daemon.register(service)?;
        Some(name)
    };

    let receiver = daemon.browse(MDNS_SERVICE_TYPE)?;
    let reg = Arc::clone(registry);
    tasks.push(tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(svc) => {
                    if let Some(peer) = peer_from_mdns(&svc) {
                        reg.upsert(peer, PeerSource::Mdns);
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    if let Some(device_id) = instance_of(&fullname) {
                        reg.mdns_removed(device_id, PEER_TIMEOUT);
                    }
                }
                _ => {}
            }
        }
    }));

    Ok((daemon, fullname))
}

/// Build a Peer from an mDNS resolution; returns None when fields are missing
/// (i.e. not a lanecho service)
fn peer_from_mdns(svc: &mdns_sd::ResolvedService) -> Option<Peer> {
    let info = PeerInfo {
        device_id: svc.get_property_val_str("id")?.to_string(),
        name: svc.get_property_val_str("name")?.to_string(),
        fingerprint: svc.get_property_val_str("fp")?.to_string(),
        platform: svc.get_property_val_str("platform")?.to_string(),
        os_version: svc.get_property_val_str("osv").map(str::to_string),
    };
    // Multiple NICs yield multiple addresses: normalize and keep them as
    // candidates (ScopedIp carries a scope id, but only the bare address is
    // taken here)
    let addrs = normalize_addrs(svc.addresses.iter().map(|ip| ip.to_ip_addr()).collect());
    if addrs.is_empty() {
        return None;
    }
    Some(Peer {
        info,
        addrs,
        port: svc.port,
    })
}

/// Normalize the candidate addresses: drop IPv6 link-local addresses that
/// cannot be dialed directly, and put non-loopback IPv4 first
///
/// The result may be empty (when the first mDNS resolution event carries only
/// AAAA records, say); the caller must then drop this update and wait for a
/// later event with a usable address — dead addresses must never go back into
/// the list.
fn normalize_addrs(all: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut addrs: Vec<IpAddr> = all.into_iter().filter(|ip| !is_link_local_v6(ip)).collect();
    // Stable sort: equal priority keeps first-seen order
    addrs.sort_by_key(|ip| (ip.is_loopback(), !ip.is_ipv4()));
    addrs
}

/// IPv6 link-local address (fe80::/10): without a scope id it cannot be dialed
/// directly, so it counts as unusable
fn is_link_local_v6(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        IpAddr::V4(_) => false,
    }
}

/// Extract the instance name (i.e. the device ID) from an mDNS fullname: strip
/// the service type suffix and the trailing separator dot
fn instance_of(fullname: &str) -> Option<&str> {
    fullname
        .strip_suffix(MDNS_SERVICE_TYPE)
        .map(|s| s.trim_end_matches('.'))
}

/// Build the multicast UDP socket: enable address reuse, then bind the
/// discovery port
///
/// SO_REUSEADDR (plus SO_REUSEPORT on unix) lets several instances on one
/// machine all receive multicast, and lets a process restart quickly without
/// waiting on TIME_WAIT — under multicast semantics every socket bound with
/// reuse gets its own copy of the packet, so none of them steal from another.
fn bind_multicast_socket(discovery_port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&std::net::SocketAddr::from((Ipv4Addr::UNSPECIFIED, discovery_port)).into())?;
    UdpSocket::from_std(sock.into())
}

/// Initialize UDP multicast: join the group and start the heartbeat and
/// receive tasks
///
/// Packets are read from the shared set (so an identity update takes effect at
/// once); in passive mode the set is empty, which silences the heartbeat and
/// the replies on its own (receive only).
async fn start_udp(
    info: &PeerInfo,
    discovery_port: u16,
    packets: SharedPackets,
    registry: &Arc<Registry>,
    tasks: &mut Vec<JoinHandle<()>>,
) -> Result<Arc<UdpSocket>, DiscoveryError> {
    let socket = bind_multicast_socket(discovery_port)?;
    socket.join_multicast_v4(MULTICAST_GROUP, Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    let socket = Arc::new(socket);

    let sock = Arc::clone(&socket);
    let reg = Arc::clone(registry);
    let self_fp = info.fingerprint.clone();
    tasks.push(tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let announce = read_packets(&packets).announce.clone();
                    if announce.is_empty() {
                        continue;
                    }
                    if let Err(e) = sock.send_to(&announce, (MULTICAST_GROUP, discovery_port)).await {
                        tracing::debug!("UDP announce 发送失败: {e}");
                    }
                }
                recv = sock.recv_from(&mut buf) => {
                    let Ok((n, src)) = recv else { continue };
                    let Ok(packet) = serde_json::from_slice::<AnnouncePacket>(&buf[..n]) else {
                        continue;
                    };
                    if packet.info.fingerprint == self_fp {
                        continue;
                    }
                    match packet.kind {
                        AnnounceKind::Goodbye => reg.remove(&packet.info.fingerprint),
                        kind => {
                            reg.upsert(
                                Peer {
                                    info: packet.info,
                                    addrs: vec![src.ip()],
                                    port: packet.tcp_port,
                                },
                                PeerSource::Udp,
                            );
                            // Reply to an announce with a unicast response so
                            // the new node sees us right away
                            if kind == AnnounceKind::Announce {
                                let response = read_packets(&packets).response.clone();
                                if !response.is_empty() {
                                    let _ = sock.send_to(&response, src).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    }));

    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a registry and event receiver for tests
    fn test_registry(self_fp: &str) -> (Arc<Registry>, mpsc::Receiver<PeerEvent>) {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAP);
        (
            Arc::new(Registry {
                self_fingerprint: self_fp.to_string(),
                peers: Mutex::new(HashMap::new()),
                events: tx,
            }),
            rx,
        )
    }

    /// Build a test node
    fn test_peer(fp: &str, name: &str) -> Peer {
        Peer {
            info: PeerInfo {
                device_id: format!("dev-{fp}"),
                name: name.to_string(),
                fingerprint: fp.to_string(),
                platform: "macos".to_string(),
                os_version: None,
            },
            addrs: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
            port: 42524,
        }
    }

    /// Standalone temporary directory, cleaned up on Drop (as in the tls.rs
    /// tests)
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lanecho-probe-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Start a TLS listener holding the given identity's certificate
    /// (simulating an online lanecho) and return its address
    async fn spawn_tls_listener(identity: &DeviceIdentity) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor =
            tokio_rustls::TlsAcceptor::from(Arc::new(tls::server_config(identity).unwrap()));
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                // The prober disconnects once the handshake completes, so the
                // result here does not matter
                tokio::spawn(async move {
                    let _ = acceptor.accept(tcp).await;
                });
            }
        });
        addr
    }

    /// TLS config for the prober (its own separate identity, accepts any
    /// server certificate)
    fn probe_config(dir: &TempDir) -> Arc<rustls::ClientConfig> {
        let id = DeviceIdentity::load_or_create(&dir.0).unwrap();
        Arc::new(tls::client_config(&id, None).unwrap())
    }

    /// A repeated heartbeat must not emit Up again; a change of info (a
    /// rename, say) must emit it once more
    #[tokio::test]
    async fn upsert_emits_only_on_change() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "old"), PeerSource::Udp);
        reg.upsert(test_peer("aaa", "old"), PeerSource::Udp); // heartbeat: unchanged
        reg.upsert(test_peer("aaa", "new"), PeerSource::Udp); // rename: changed
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.info.name == "old"));
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.info.name == "new"));
        assert!(rx.try_recv().is_err());
    }

    /// Our own packets must be filtered out
    #[tokio::test]
    async fn self_is_filtered() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("self", "me"), PeerSource::Udp);
        assert!(rx.try_recv().is_err());
        assert!(reg.snapshot().is_empty());
    }

    /// Address merging across channels: a new address emits one Up, a
    /// single-address heartbeat no longer makes it jitter, and the order
    /// stays stable
    #[tokio::test]
    async fn upsert_merges_addrs_stably() {
        let (reg, mut rx) = test_registry("self");
        let addr_a = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let addr_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        // UDP reports the single address A first
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.addrs == vec![addr_a]));

        // mDNS reports [B, A]: merged onto the existing order as [A, B], and
        // the new address B emits Up
        let mut peer = test_peer("aaa", "n");
        peer.addrs = vec![addr_b, addr_a];
        reg.upsert(peer, PeerSource::Mdns);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.addrs == vec![addr_a, addr_b]));

        // A UDP heartbeat reports A again: the merge result is unchanged, so
        // no further event may be emitted
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        assert!(rx.try_recv().is_err());
    }

    /// remove emits Down only for a node that exists
    #[tokio::test]
    async fn remove_emits_down_once() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("bbb", "b"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.remove("bbb");
        reg.remove("bbb"); // already gone, must not emit again
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "bbb"));
        assert!(rx.try_recv().is_err());
    }

    /// Regression: when only mDNS gets through (UDP multicast blocked by the
    /// network) a node must not be kicked by the heartbeat timeout — mDNS has
    /// no periodic event to refresh the timestamp
    #[tokio::test]
    async fn mdns_peer_survives_sweep() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Mdns);
        let _ = rx.try_recv();
        // timeout of 0: everything judged dead by elapsed time gets kicked,
        // an mDNS-alive node must survive
        reg.sweep(Duration::ZERO);
        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
    }

    /// A UDP-only node is cleaned up once its heartbeat times out
    #[tokio::test]
    async fn udp_peer_swept_after_timeout() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.sweep(Duration::ZERO);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "aaa"));
        assert!(reg.snapshot().is_empty());
    }

    /// mDNS service gone with no UDP heartbeat: the node goes offline at once
    #[tokio::test]
    async fn mdns_removed_downs_peer_without_udp() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Mdns);
        let _ = rx.try_recv();
        reg.mdns_removed("dev-aaa", PEER_TIMEOUT);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "aaa"));
        assert!(reg.snapshot().is_empty());
    }

    /// mDNS gone but the UDP heartbeat still fresh: the node is kept, and
    /// only goes offline in a sweep once UDP times out too
    #[tokio::test]
    async fn mdns_removed_keeps_peer_with_live_udp() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Mdns);
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        let _ = rx.try_recv();
        // UDP heartbeat inside the timeout window: stays online
        reg.mdns_removed("dev-aaa", PEER_TIMEOUT);
        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
        // once UDP times out as well (timeout=0) the sweep finishes it off
        reg.sweep(Duration::ZERO);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "aaa"));
    }

    /// UDP packet serialization roundtrip
    #[test]
    fn announce_packet_roundtrip() {
        let packet = AnnouncePacket {
            kind: AnnounceKind::Announce,
            info: test_peer("ccc", "c").info,
            tcp_port: 42524,
        };
        let bytes = serde_json::to_vec(&packet).unwrap();
        let back: AnnouncePacket = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.kind, AnnounceKind::Announce);
        assert_eq!(back.info.fingerprint, "ccc");
        assert_eq!(back.tcp_port, 42524);
    }

    /// Address normalization: fe80:: link-local is dropped and non-loopback
    /// IPv4 comes first; the result is empty when only link-local remains
    #[test]
    fn normalize_addrs_filters_and_sorts() {
        use std::net::Ipv6Addr;
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let lo4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let ll6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        let lo6 = IpAddr::V6(Ipv6Addr::LOCALHOST);

        // link-local dropped; non-loopback IPv4 > loopback IPv4 > loopback IPv6
        assert_eq!(normalize_addrs(vec![ll6, lo6, lo4, v4]), vec![v4, lo4, lo6]);
        // only link-local left: returns empty (the caller drops the update)
        assert_eq!(normalize_addrs(vec![ll6]), Vec::<IpAddr>::new());
    }

    /// A node must not be reported when the first mDNS event carries only
    /// fe80::; it is reported once an IPv4 event arrives, and fe80:: must
    /// never end up in the candidate list
    #[tokio::test]
    async fn link_local_only_peer_is_deferred() {
        use std::net::Ipv6Addr;
        let (reg, mut rx) = test_registry("self");
        let ll6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        // link-local address only: not registered, no event
        let mut peer = test_peer("aaa", "n");
        peer.addrs = vec![ll6];
        reg.upsert(peer, PeerSource::Mdns);
        assert!(rx.try_recv().is_err());
        assert!(reg.snapshot().is_empty());

        // IPv4 arrives: reported normally, the list holds only usable addresses
        let mut peer = test_peer("aaa", "n");
        peer.addrs = vec![ll6, v4];
        reg.upsert(peer, PeerSource::Mdns);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.addrs == vec![v4]));
    }

    /// Extract the instance (device ID) from an mDNS fullname
    #[test]
    fn instance_extraction() {
        assert_eq!(
            instance_of("uuid-1234._lanecho._tcp.local."),
            Some("uuid-1234")
        );
        assert_eq!(instance_of("._lanecho._tcp.local."), Some(""));
    }

    /// sweep only picks nodes that are mDNS-only alive with UDP silent and
    /// past the throttle interval
    #[tokio::test]
    async fn sweep_flags_stale_mdns_only_peers_for_probe() {
        let (reg, _rx) = test_registry("self");
        // aaa: mDNS-only — the probe target (coming online counts as just
        // probed, so clear the timestamp to simulate a full interval)
        reg.upsert(test_peer("aaa", "a"), PeerSource::Mdns);
        // bbb: UDP heartbeat is fresh — not probed
        reg.upsert(test_peer("bbb", "b"), PeerSource::Udp);
        reg.lock_peers().get_mut("aaa").unwrap().last_probe = None;

        let probes = reg.sweep(PEER_TIMEOUT);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].info.fingerprint, "aaa");
        // the timestamp is stamped: the next sweep must not probe again
        assert!(reg.sweep(PEER_TIMEOUT).is_empty());
        // the probe target is not cleaned up (still alive over mDNS)
        assert_eq!(reg.snapshot().len(), 2);
    }

    /// Applying a failed probe: a still-fresh UDP heartbeat is newer evidence
    /// and keeps the node, while an mDNS-only node is removed
    #[tokio::test]
    async fn probe_failed_respects_fresh_udp() {
        let (reg, mut rx) = test_registry("self");
        // both channels alive: equivalent to "UDP came back inside the 2s
        // probe window", so the node is kept
        reg.upsert(test_peer("aaa", "a"), PeerSource::Mdns);
        reg.upsert(test_peer("aaa", "a"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.probe_failed("aaa", PEER_TIMEOUT);
        assert_eq!(reg.snapshot().len(), 1);
        assert!(rx.try_recv().is_err());

        // mDNS only: a failed probe takes it offline
        reg.upsert(test_peer("ccc", "c"), PeerSource::Mdns);
        let _ = rx.try_recv();
        reg.probe_failed("ccc", PEER_TIMEOUT);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "ccc"));
        assert_eq!(reg.snapshot().len(), 1);
    }

    /// End to end (real TCP + TLS): a node listening with its own certificate
    /// survives the probe, a node whose listener is gone is removed
    #[tokio::test]
    async fn probe_keeps_live_and_removes_dead() {
        let (reg, mut rx) = test_registry("self");
        let (live_dir, prober_dir) = (TempDir::new(), TempDir::new());

        // live node: a real TLS listener with its own identity certificate,
        // its table fingerprint matching that certificate
        let live_id = DeviceIdentity::load_or_create(&live_dir.0).unwrap();
        let live_addr = spawn_tls_listener(&live_id).await;
        let mut live = test_peer(&live_id.fingerprint, "live");
        live.addrs = vec![live_addr.ip()];
        live.port = live_addr.port();
        reg.upsert(live.clone(), PeerSource::Mdns);

        // dead node: bind a port then close it right away (simulating the OS
        // reclaiming the port after a process crash)
        let dead_port = {
            let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            l.local_addr().unwrap().port()
        };
        let mut dead = test_peer("bbb", "dead");
        dead.addrs = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        dead.port = dead_port;
        reg.upsert(dead.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        // daemon is None: exercises the direct-removal fallback path taken
        // when mDNS is unavailable
        let config = probe_config(&prober_dir);
        probe_peer(Arc::clone(&reg), None, Some(Arc::clone(&config)), live).await;
        probe_peer(Arc::clone(&reg), None, Some(config), dead).await;

        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "bbb"));
        let snapshot = reg.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].info.fingerprint, live_id.fingerprint);
    }

    /// Regression: a stale address now points at a different device (an
    /// imposter) — TCP connects but the certificate fingerprint does not
    /// match, so it **must not** count as alive. Deciding liveness on a bare
    /// connect leaves a disconnected node online forever in everyone else's
    /// list, while the sync layer, which pins the fingerprint, fails every
    /// time, producing the split of "online in the list yet sync never gets
    /// through"
    #[tokio::test]
    async fn probe_rejects_imposter_listener() {
        let (reg, mut rx) = test_registry("self");
        let (imposter_dir, prober_dir) = (TempDir::new(), TempDir::new());

        // imposter: a TLS listener holding **another** identity's certificate
        // (as if DHCP had reassigned the disconnected node's old address to
        // this machine, which also runs lanecho)
        let imposter_id = DeviceIdentity::load_or_create(&imposter_dir.0).unwrap();
        let addr = spawn_tls_listener(&imposter_id).await;

        // the node in the table claims that address, but its fingerprint is
        // its own (different from the imposter's)
        let mut victim = test_peer("victim-fp", "victim");
        victim.addrs = vec![addr.ip()];
        victim.port = addr.port();
        reg.upsert(victim.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        probe_peer(
            Arc::clone(&reg),
            None,
            Some(probe_config(&prober_dir)),
            victim,
        )
        .await;

        // reaching the imposter does not count as alive: with no daemon this
        // takes the direct-removal fallback
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "victim-fp"));
        assert!(reg.snapshot().is_empty());
    }

    /// With the daemon available, a failed probe must not remove the node
    /// directly: the death decision is left to the ServiceRemoved that verify
    /// triggers (node table and daemon cache are cleaned together, otherwise
    /// a peer reconnecting before the cache expires stays invisible forever,
    /// because an unchanged record emits no event)
    #[tokio::test]
    async fn probe_failure_with_daemon_defers_to_verify() {
        let (reg, mut rx) = test_registry("self");
        let prober_dir = TempDir::new();
        let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
            eprintln!("跳过: 本环境无法创建 mDNS daemon");
            return;
        };

        // dead port: bind then close immediately
        let dead_port = {
            let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            l.local_addr().unwrap().port()
        };
        let mut dead = test_peer("aaa", "dead");
        dead.addrs = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        dead.port = dead_port;
        reg.upsert(dead.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        probe_peer(
            Arc::clone(&reg),
            Some(daemon.clone()),
            Some(probe_config(&prober_dir)),
            dead,
        )
        .await;

        // the node is still in the table with no Down event: removal can only
        // come from ServiceRemoved
        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
        let _ = daemon.shutdown();
    }
}
