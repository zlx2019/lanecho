//! 同步引擎: 配对模型 + 组内广播 + LWW 决胜 + 回声抑制(方案第 6 节)
//!
//! 引擎不直接触碰系统剪贴板:
//! - 输入是 [`ClipboardEvent`] 流 —— 生产装配接 [`crate::clipboard::spawn_watcher`],
//!   回环测试可直接注入事件;
//! - 远端同步的落地以 [`EngineEvent::ApplyRemote`] 事件交由装配层执行写入,
//!   回声哈希在发出事件**之前**登记(写入先于 watcher 检测, 顺序有保证)。
//!
//! 回声抑制铁律(方案 6.4): 远端同步写入的内容永不广播; 从历史面板
//! "选中复制"属显式用户意图, 走正常复制路径照常广播。

mod net;
mod paired;

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

use crate::clipboard::{ClipboardContent, ClipboardEvent, hash_text};
use crate::config::{
    ECHO_RECENT_CAP, EVENT_CHANNEL_CAP, MAX_SYNC_TEXT_BYTES, PAIR_DECISION_TIMEOUT,
};
use crate::discovery::{DiscoveryError, DiscoveryService, Peer, PeerEvent};
use crate::identity::{DeviceIdentity, IdentityError};
use crate::protocol::{PeerInfo, ProtocolError, content_type, reason_code};
use crate::tls::{self, TlsError};

/// 同步引擎错误
#[derive(Debug, Error)]
pub enum SyncError {
    /// 发现层错误
    #[error("发现服务错误: {0}")]
    Discovery(#[from] DiscoveryError),
    /// 身份层错误
    #[error("设备身份错误: {0}")]
    Identity(#[from] IdentityError),
    /// TLS 配置错误
    #[error("TLS 错误: {0}")]
    Tls(#[from] TlsError),
    /// 协议层错误(编解码/版本/乱序)
    #[error("协议错误: {0}")]
    Protocol(#[from] ProtocolError),
    /// 底层 IO 错误
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 对端不可达(不在线或全部候选地址连接失败)
    #[error("对端不可达")]
    PeerUnreachable,
    /// 对端 TLS 证书与其声明的指纹不一致(疑似冒充)
    #[error("对端指纹与声明不一致")]
    FingerprintMismatch,
    /// 对端用户拒绝了配对请求
    #[error("对端拒绝配对")]
    PairRejected,
    /// 同步被对端拒绝(参数为结构化拒因码)
    #[error("对端拒绝同步: {0}")]
    Rejected(String),
    /// 等待对端应答超时(参数标注等的是哪一步)
    #[error("等待 {0} 超时")]
    Timeout(&'static str),
}

/// 引擎配置
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// 引擎数据目录(身份文件与 paired.json 所在)
    pub data_dir: PathBuf,
    /// TCP 监听端口(0 = 随机分配, 测试与同机多实例用)
    pub tcp_port: u16,
    /// UDP 组播发现端口
    pub discovery_port: u16,
    /// 发现层隐身(只收不发)
    pub passive: bool,
    /// 同步开关初始值(熔断闸, 运行时可切)
    pub sync_enabled: bool,
}

/// 引擎事件: UI/CLI 的唯一信息来源
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// 节点上线或信息更新
    PeerUp(Peer),
    /// 节点下线(参数为指纹)
    PeerDown(String),
    /// 收到入站配对请求, 上层弹窗后经 [`SyncEngine::respond_pair`] 回填决定
    PairRequested {
        /// 请求方设备信息
        peer: PeerInfo,
    },
    /// 配对成立(双向; 主动配对成功与被动接受均触发)
    Paired {
        /// 对端设备信息
        peer: PeerInfo,
    },
    /// 配对解除(本地操作或对端通知)
    Unpaired {
        /// 对端指纹
        fingerprint: String,
    },
    /// 本机用户复制了新内容(历史管道与 UI 提示挂此事件)
    LocalCopied {
        /// 剪贴板内容
        content: ClipboardContent,
        /// 内容哈希
        hash: String,
        /// 复制时刻(Unix 毫秒)
        timestamp_ms: u64,
    },
    /// 远端同步已受理, 装配层应将文本写入系统剪贴板
    ///
    /// **契约**: 引擎在发出本事件前已登记回声哈希(假定写入必然发生);
    /// 装配层写入失败时必须调用 [`SyncEngine::cancel_echo`] 撤销登记,
    /// 否则残留的孤儿哈希会误吞下一次同内容的真实本机复制。
    ApplyRemote {
        /// 同步来的文本(逐字节原样)
        text: String,
        /// 来源设备
        from: PeerInfo,
        /// 对端复制时刻(Unix 毫秒)
        timestamp_ms: u64,
        /// 已登记的回声哈希(写入失败时凭此撤销)
        hash: String,
    },
    /// 一次对外同步的结果(逐目标节点上报)
    SyncSent {
        /// 目标设备
        to: PeerInfo,
        /// Ok 即送达; Err 为拒因或错误描述
        result: Result<(), String>,
    },
}

/// 待决配对请求: 决策通道 + 请求方信息(启动兜底拉取用)
///
/// `generation` 区分同指纹的并发请求: 后到顶替先到时, 先到者退出清理
/// 只删自己那一代, 不得误删顶替者刚插入的句柄。
struct PendingPair {
    /// 请求代数(单调递增)
    generation: u64,
    /// 请求方设备信息
    peer: PeerInfo,
    /// 决策通道(UI 经 respond_pair 回填)
    tx: oneshot::Sender<bool>,
}

/// 引擎共享状态(网络会话与各泵任务共用)
pub(crate) struct Inner {
    /// 本机身份(Mutex<Arc> 支持改名时快照替换 —— 指纹/证书不变, 仅展示名)
    identity: Mutex<Arc<DeviceIdentity>>,
    /// 引擎数据目录(身份/配对文件所在; 改名持久化用)
    data_dir: PathBuf,
    /// 服务端 TLS 配置(全部入站连接共享; 证书不随改名变, 无需重建)
    pub(crate) server_tls: Arc<rustls::ServerConfig>,
    /// 发现服务
    discovery: DiscoveryService,
    /// 配对集合
    paired: Mutex<paired::PairedStore>,
    /// 配对表落盘串行锁(锁内取最新快照, 防并发写乱序回退 —— M3 HistoryStore 同款)
    paired_io: Arc<tokio::sync::Mutex<()>>,
    /// 引擎事件发送端
    events: mpsc::Sender<EngineEvent>,
    /// 对外同步的单调序号(日志排查用)
    seq: AtomicU64,
    /// 本机最近一次复制时刻(Unix 毫秒), LWW 决胜基准
    last_local_copy_ms: AtomicU64,
    /// 最近远端写入的内容哈希(回声登记, 一次性消费)
    echo: Mutex<VecDeque<String>>,
    /// 同步开关(熔断闸)
    sync_enabled: AtomicBool,
    /// 等待 UI 决策的入站配对请求(指纹 → 待决记录)
    pending_pairs: Mutex<HashMap<String, PendingPair>>,
    /// 配对请求代数计数(见 [`PendingPair::generation`])
    pending_seq: AtomicU64,
    /// 实际监听端口
    port: u16,
}

impl Inner {
    /// 当前身份快照(Arc 克隆, 廉价)
    pub(crate) fn current_identity(&self) -> Arc<DeviceIdentity> {
        Arc::clone(&self.identity.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// 取配对表锁(毒锁恢复内部数据)
    fn lock_paired(&self) -> MutexGuard<'_, paired::PairedStore> {
        self.paired.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 取回声表锁
    fn lock_echo(&self) -> MutexGuard<'_, VecDeque<String>> {
        self.echo.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 取待决配对表锁
    fn lock_pending(&self) -> MutexGuard<'_, HashMap<String, PendingPair>> {
        self.pending_pairs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// 发引擎事件; 消费端关闭时静默丢弃(引擎随后会被 shutdown)
    pub(crate) async fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event).await;
    }

    /// 指纹是否已配对
    fn is_paired(&self, fingerprint: &str) -> bool {
        self.lock_paired().contains(fingerprint)
    }

    /// 全部已配对指纹(广播目标筛选用)
    fn paired_fingerprints(&self) -> HashSet<String> {
        self.lock_paired()
            .list()
            .into_iter()
            .map(|p| p.fingerprint)
            .collect()
    }

    /// 写入配对(幂等)+ 落盘 + 事件
    pub(crate) async fn add_paired(self: &Arc<Self>, info: &PeerInfo) {
        self.lock_paired().insert(info);
        self.persist_paired().await;
        self.emit(EngineEvent::Paired { peer: info.clone() }).await;
    }

    /// 移除配对 + 落盘 + 事件(不存在时为空操作)
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

    /// 配对表落盘: io 串行 + 锁内取最新快照
    ///
    /// 并发变更时若各自携带快照落盘, 完成顺序无保证, 旧快照可能覆盖新
    /// 快照 —— 最坏是 unpair 掉的对端在重启后"复活"(接收侧安全边界回退)。
    /// 串行锁内再取快照保证写盘内容单调向前。
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

    /// 入站配对判定: 已配对幂等接受; 否则上抛 UI 弹窗等用户决定
    ///
    /// 同一对端并发重复请求时, 后到顶替先到(先到的等待者收到通道
    /// 关闭按拒绝处理); 先到者退出清理凭代数只删自己那一项。
    pub(crate) async fn decide_pair(self: &Arc<Self>, remote: &PeerInfo) -> bool {
        if self.is_paired(&remote.fingerprint) {
            return true;
        }
        let generation = self.pending_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.lock_pending().insert(
            remote.fingerprint.clone(),
            PendingPair {
                generation,
                peer: remote.clone(),
                tx,
            },
        );
        self.emit(EngineEvent::PairRequested {
            peer: remote.clone(),
        })
        .await;
        let accepted = matches!(
            tokio::time::timeout(PAIR_DECISION_TIMEOUT, rx).await,
            Ok(Ok(true))
        );
        {
            // 只清理自己那一代: 已被并发请求顶替时, 表里是顶替者的句柄
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

    /// 入站同步检查链(方案 6.1): 熔断 → 配对 → 类型 → 大小 → LWW
    ///
    /// 通过后**先登记回声哈希再发 ApplyRemote**(装配层写剪贴板早于
    /// watcher 下轮检测, 该顺序保证回声必被吞); LWW 忽略时收下回 Ok
    /// —— 对端无需区分"已应用"与"本机更新而未应用"。
    pub(crate) async fn accept_sync(
        &self,
        remote: &PeerInfo,
        timestamp_ms: u64,
        content_kind: &str,
        data: String,
    ) -> Result<(), &'static str> {
        if !self.sync_enabled.load(Ordering::Relaxed) {
            return Err(reason_code::DISABLED);
        }
        if !self.is_paired(&remote.fingerprint) {
            return Err(reason_code::NOT_PAIRED);
        }
        if content_kind != content_type::TEXT {
            return Err(reason_code::UNSUPPORTED_TYPE);
        }
        if data.len() > MAX_SYNC_TEXT_BYTES {
            return Err(reason_code::TOO_LARGE);
        }
        // LWW(决策 #7): 本机复制时刻更新(含相等, 保守少一次覆盖)则忽略。
        // info 级并携带双方时刻: 时钟漂移会表现为"单向持续拒收", 留排查线索
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
            text: data,
            from: remote.clone(),
            timestamp_ms,
            hash,
        })
        .await;
        Ok(())
    }

    /// 登记一次远端写入的回声哈希(容量满时淘汰最旧)
    ///
    /// 同哈希只登记一份: watcher 对同内容去重后只产生一次绕回事件, 重复
    /// 登记(多台远端先后同步同一文本)会残留孤儿, 误吞后续真实本机复制。
    fn push_echo(&self, hash: String) {
        let mut echo = self.lock_echo();
        if echo.iter().any(|h| *h == hash) {
            return;
        }
        if echo.len() >= ECHO_RECENT_CAP {
            echo.pop_front();
        }
        echo.push_back(hash);
    }

    /// 回声命中即消费(一次性): 命中返回 true, 调用方跳过该剪贴板事件
    fn take_echo(&self, hash: &str) -> bool {
        let mut echo = self.lock_echo();
        match echo.iter().position(|h| h == hash) {
            Some(idx) => {
                echo.remove(idx);
                true
            }
            None => false,
        }
    }

    /// 把一条本机文本广播给"已配对且在线"的全部节点(逐节点并发拨号)
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
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        for peer in targets {
            let inner = Arc::clone(self);
            let text = text.clone();
            tokio::spawn(async move {
                let identity = inner.current_identity();
                let result = net::sync_transaction(&identity, &peer, seq, timestamp_ms, text)
                    .await
                    .map_err(|e| e.to_string());
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

/// 同步引擎句柄: 启动后台任务后对外提供控制面
pub struct SyncEngine {
    /// 共享状态
    inner: Arc<Inner>,
    /// 后台任务句柄(shutdown 时中止)
    tasks: Vec<JoinHandle<()>>,
}

impl SyncEngine {
    /// 启动引擎: 加载身份、绑定监听、启动发现与各泵任务
    ///
    /// `clipboard_rx` 是剪贴板变化事件的入口 —— 生产装配传
    /// [`crate::clipboard::spawn_watcher`] 的接收端, 测试可自行注入。
    pub async fn start(
        config: EngineConfig,
        clipboard_rx: mpsc::Receiver<ClipboardEvent>,
    ) -> Result<(Self, mpsc::Receiver<EngineEvent>), SyncError> {
        let identity = Arc::new(DeviceIdentity::load_or_create(&config.data_dir)?);
        let server_tls = Arc::new(tls::server_config(&identity)?);
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.tcp_port)).await?;
        let port = listener.local_addr()?.port();
        let (discovery, peer_rx) = DiscoveryService::start(
            identity.peer_info(),
            port,
            config.discovery_port,
            config.passive,
        )
        .await?;
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
            sync_enabled: AtomicBool::new(config.sync_enabled),
            pending_pairs: Mutex::new(HashMap::new()),
            pending_seq: AtomicU64::new(0),
            port,
        });
        let tasks = vec![
            tokio::spawn(net::accept_loop(Arc::clone(&inner), listener)),
            spawn_discovery_pump(Arc::clone(&inner), peer_rx),
            spawn_clipboard_pump(Arc::clone(&inner), clipboard_rx),
        ];
        Ok((Self { inner, tasks }, events_rx))
    }

    /// 本机设备信息
    pub fn local_info(&self) -> PeerInfo {
        self.inner.current_identity().peer_info()
    }

    /// 热更新展示名并即时重新广播(None = 恢复跟随 hostname)
    ///
    /// 指纹与证书不变(设备身份不变), 仅 identity.json 的展示名持久化
    /// 并替换内存快照; 发现层经 update_info 热广播, 对端无"下线再上线"。
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

    /// 实际监听端口(配置 0 时为随机分配结果)
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// 主动向指定节点发起配对(阻塞至对端用户决策或超时)
    pub async fn pair(&self, fingerprint: &str) -> Result<(), SyncError> {
        let peer = self
            .inner
            .discovery
            .peer_by_fingerprint(fingerprint)
            .ok_or(SyncError::PeerUnreachable)?;
        let identity = self.inner.current_identity();
        let remote = net::pair_transaction(&identity, &peer).await?;
        self.inner.add_paired(&remote).await;
        Ok(())
    }

    /// 回填入站配对请求的用户决定(对应 [`EngineEvent::PairRequested`])
    pub fn respond_pair(&self, fingerprint: &str, accept: bool) {
        if let Some(pending) = self.inner.lock_pending().remove(fingerprint) {
            let _ = pending.tx.send(accept);
        }
    }

    /// 等待 UI 决策的入站配对请求快照
    ///
    /// 装配层启动兜底用: 事件泵早于前端就绪, 窗口期内到达的
    /// PairRequested 事件无人监听即丢, 前端挂载时凭此补拉。
    pub fn pending_pair_requests(&self) -> Vec<PeerInfo> {
        self.inner
            .lock_pending()
            .values()
            .map(|p| p.peer.clone())
            .collect()
    }

    /// 解除配对: 本地立即生效(安全边界), 并尽力通知对端
    pub async fn unpair(&self, fingerprint: &str) {
        let peer = self.inner.discovery.peer_by_fingerprint(fingerprint);
        self.inner.remove_paired(fingerprint).await;
        if let Some(peer) = peer {
            let identity = self.inner.current_identity();
            if let Err(e) = net::unpair_transaction(&identity, &peer).await {
                tracing::debug!("解除配对通知发送失败(对端下次同步时会被拒): {e}");
            }
        }
    }

    /// 切换同步开关(熔断闸): 关闭后不广播也不受理入站同步
    pub fn set_sync_enabled(&self, enabled: bool) {
        self.inner.sync_enabled.store(enabled, Ordering::Relaxed);
    }

    /// 撤销一条回声登记(装配层写剪贴板失败时调用, 见 [`EngineEvent::ApplyRemote`] 契约)
    pub fn cancel_echo(&self, hash: &str) {
        if self.inner.take_echo(hash) {
            tracing::debug!("剪贴板写入失败, 已撤销对应的回声登记");
        }
    }

    /// 当前在线节点快照
    pub fn peers(&self) -> Vec<Peer> {
        self.inner.discovery.peers()
    }

    /// 当前配对清单
    pub fn paired_list(&self) -> Vec<PairedPeer> {
        self.inner.lock_paired().list()
    }

    /// 优雅关闭: 发现层 goodbye + 停掉全部后台任务
    pub async fn shutdown(&self) {
        self.inner.discovery.shutdown().await;
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// 发现事件泵: PeerEvent → EngineEvent 透传
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

/// 剪贴板事件泵: 回声过滤 → LWW 基准更新 → 历史/UI 事件 → 文本广播
fn spawn_clipboard_pump(
    inner: Arc<Inner>,
    mut clipboard_rx: mpsc::Receiver<ClipboardEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = clipboard_rx.recv().await {
            // 回声: 远端写入经系统剪贴板绕回, 一次性吞掉;
            // 不更新 LWW 基准 —— 它不是本机用户的复制动作
            if inner.take_echo(&event.hash) {
                tracing::debug!("回声抑制: 跳过远端写入的绕回事件");
                continue;
            }
            inner
                .last_local_copy_ms
                .store(event.timestamp_ms, Ordering::Relaxed);
            // v1 仅文本参与跨设备同步(决策 #9); 历史管道消费 LocalCopied 全类型。
            // 上限/开关检查先于 clone: 不为注定丢弃的文本白拷整段
            let text = match &event.content {
                ClipboardContent::Text(text) if inner.sync_enabled.load(Ordering::Relaxed) => {
                    if text.len() > MAX_SYNC_TEXT_BYTES {
                        tracing::info!(
                            bytes = text.len(),
                            "文本超过同步上限, 不广播(本机历史不受影响)"
                        );
                        None
                    } else {
                        Some(text.clone())
                    }
                }
                _ => None,
            };
            inner
                .emit(EngineEvent::LocalCopied {
                    content: event.content,
                    hash: event.hash,
                    timestamp_ms: event.timestamp_ms,
                })
                .await;
            let Some(text) = text else {
                continue;
            };
            // 帧上限精确校验: JSON 转义最坏使文本膨胀 6 倍(控制字符 \uXXXX),
            // 裸字节检查不足以保证成帧 <1MiB; 仅对可能越界的大文本预序列化精判,
            // 否则 write_frame 阶段才失败, 该条同步会对全部对端静默丢失
            const SYNC_FRAME_OVERHEAD: usize = 256;
            let frame_budget = crate::protocol::MAX_FRAME_LEN as usize - SYNC_FRAME_OVERHEAD;
            if text.len() > frame_budget / 6
                && serde_json::to_string(&text)
                    .map(|s| s.len())
                    .unwrap_or(usize::MAX)
                    > frame_budget
            {
                tracing::info!(
                    bytes = text.len(),
                    "文本转义后超过协议帧上限, 不广播(本机历史不受影响)"
                );
                continue;
            }
            inner.broadcast_text(text, event.timestamp_ms).await;
        }
    })
}

#[cfg(test)]
mod tests;
