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
    ApplyRemote {
        /// 同步来的文本(逐字节原样)
        text: String,
        /// 来源设备
        from: PeerInfo,
        /// 对端复制时刻(Unix 毫秒)
        timestamp_ms: u64,
    },
    /// 一次对外同步的结果(逐目标节点上报)
    SyncSent {
        /// 目标设备
        to: PeerInfo,
        /// Ok 即送达; Err 为拒因或错误描述
        result: Result<(), String>,
    },
}

/// 引擎共享状态(网络会话与各泵任务共用)
pub(crate) struct Inner {
    /// 本机身份
    pub(crate) identity: Arc<DeviceIdentity>,
    /// 服务端 TLS 配置(全部入站连接共享)
    pub(crate) server_tls: Arc<rustls::ServerConfig>,
    /// 发现服务
    discovery: DiscoveryService,
    /// 配对集合
    paired: Mutex<paired::PairedStore>,
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
    /// 等待 UI 决策的入站配对请求(指纹 → 决策通道)
    pending_pairs: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    /// 实际监听端口
    port: u16,
}

impl Inner {
    /// 取配对表锁(毒锁恢复内部数据)
    fn lock_paired(&self) -> MutexGuard<'_, paired::PairedStore> {
        self.paired.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 取回声表锁
    fn lock_echo(&self) -> MutexGuard<'_, VecDeque<String>> {
        self.echo.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 取待决配对表锁
    fn lock_pending(&self) -> MutexGuard<'_, HashMap<String, oneshot::Sender<bool>>> {
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
    pub(crate) async fn add_paired(&self, info: &PeerInfo) {
        let (path, list) = self.lock_paired().insert(info);
        paired::persist(path, list);
        self.emit(EngineEvent::Paired { peer: info.clone() }).await;
    }

    /// 移除配对 + 落盘 + 事件(不存在时为空操作)
    pub(crate) async fn remove_paired(&self, fingerprint: &str) {
        let (existed, path, list) = self.lock_paired().remove(fingerprint);
        if !existed {
            return;
        }
        paired::persist(path, list);
        self.emit(EngineEvent::Unpaired {
            fingerprint: fingerprint.to_string(),
        })
        .await;
    }

    /// 入站配对判定: 已配对幂等接受; 否则上抛 UI 弹窗等用户决定
    ///
    /// 同一对端并发重复请求时, 后到顶替先到(先到的等待者收到通道
    /// 关闭按拒绝处理)—— 5 分钟窗口内的并发重试, 罕见且无害。
    pub(crate) async fn decide_pair(&self, remote: &PeerInfo) -> bool {
        if self.is_paired(&remote.fingerprint) {
            return true;
        }
        let (tx, rx) = oneshot::channel();
        self.lock_pending().insert(remote.fingerprint.clone(), tx);
        self.emit(EngineEvent::PairRequested {
            peer: remote.clone(),
        })
        .await;
        let accepted = matches!(
            tokio::time::timeout(PAIR_DECISION_TIMEOUT, rx).await,
            Ok(Ok(true))
        );
        self.lock_pending().remove(&remote.fingerprint);
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
        // LWW(决策 #7): 本机复制时刻更新(含相等, 保守少一次覆盖)则忽略
        if timestamp_ms <= self.last_local_copy_ms.load(Ordering::Relaxed) {
            tracing::debug!(from = %remote.name, "LWW: 本机剪贴板更新, 忽略远端同步");
            return Ok(());
        }
        self.push_echo(hash_text(&data));
        self.emit(EngineEvent::ApplyRemote {
            text: data,
            from: remote.clone(),
            timestamp_ms,
        })
        .await;
        Ok(())
    }

    /// 登记一次远端写入的回声哈希(容量满时淘汰最旧)
    fn push_echo(&self, hash: String) {
        let mut echo = self.lock_echo();
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
                let result = net::sync_transaction(&inner.identity, &peer, seq, timestamp_ms, text)
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
            identity,
            server_tls,
            discovery,
            paired: Mutex::new(paired::PairedStore::load(&config.data_dir)),
            events: events_tx,
            seq: AtomicU64::new(0),
            last_local_copy_ms: AtomicU64::new(0),
            echo: Mutex::new(VecDeque::new()),
            sync_enabled: AtomicBool::new(config.sync_enabled),
            pending_pairs: Mutex::new(HashMap::new()),
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
        self.inner.identity.peer_info()
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
        let remote = net::pair_transaction(&self.inner.identity, &peer).await?;
        self.inner.add_paired(&remote).await;
        Ok(())
    }

    /// 回填入站配对请求的用户决定(对应 [`EngineEvent::PairRequested`])
    pub fn respond_pair(&self, fingerprint: &str, accept: bool) {
        if let Some(tx) = self.inner.lock_pending().remove(fingerprint) {
            let _ = tx.send(accept);
        }
    }

    /// 解除配对: 本地立即生效(安全边界), 并尽力通知对端
    pub async fn unpair(&self, fingerprint: &str) {
        let peer = self.inner.discovery.peer_by_fingerprint(fingerprint);
        self.inner.remove_paired(fingerprint).await;
        if let Some(peer) = peer
            && let Err(e) = net::unpair_transaction(&self.inner.identity, &peer).await
        {
            tracing::debug!("解除配对通知发送失败(对端下次同步时会被拒): {e}");
        }
    }

    /// 切换同步开关(熔断闸): 关闭后不广播也不受理入站同步
    pub fn set_sync_enabled(&self, enabled: bool) {
        self.inner.sync_enabled.store(enabled, Ordering::Relaxed);
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
            // v1 仅文本参与跨设备同步(决策 #9); 历史管道消费 LocalCopied 全类型
            let text = match &event.content {
                ClipboardContent::Text(text) => Some(text.clone()),
                _ => None,
            };
            inner
                .emit(EngineEvent::LocalCopied {
                    content: event.content,
                    hash: event.hash,
                    timestamp_ms: event.timestamp_ms,
                })
                .await;
            if !inner.sync_enabled.load(Ordering::Relaxed) {
                continue;
            }
            let Some(text) = text else {
                continue;
            };
            if text.len() > MAX_SYNC_TEXT_BYTES {
                tracing::info!(
                    bytes = text.len(),
                    "文本超过同步上限, 不广播(本机历史不受影响)"
                );
                continue;
            }
            inner.broadcast_text(text, event.timestamp_ms).await;
        }
    })
}

#[cfg(test)]
mod tests;
