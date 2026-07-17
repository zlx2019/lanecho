//! 引擎桥接: 启动 SyncEngine 并把 EngineEvent 泵成 Tauri 事件
//!
//! 装配职责(方案 6.1 的装配层):
//! - 接管系统剪贴板监视(spawn_watcher → 引擎)
//! - 远端同步落地: ApplyRemote → 写系统剪贴板 + 可选通知
//! - 事件转发: 引擎事件 → 前端 Tauri 事件(事件名见 [`events`])

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;

use lanecho_core::DEFAULT_DISCOVERY_PORT;
use lanecho_core::clipboard::{self, ClipboardContent, now_ms};
use lanecho_core::protocol::PeerInfo;
use lanecho_core::sync::{EngineConfig, EngineEvent, SyncEngine};

use crate::history::{HistoryConfig, HistoryStore};
use crate::locale;
use crate::settings::Settings;
use crate::state::{AppState, lock};

/// 前端事件名(**改名必须与前端 src/events.ts 两端同步**)
pub mod events {
    /// 节点上线/信息更新, payload: PeerDto
    pub const PEER_UP: &str = "peer-up";
    /// 节点下线, payload: 指纹字符串
    pub const PEER_DOWN: &str = "peer-down";
    /// 收到配对请求, payload: PeerDto
    pub const PAIR_REQUESTED: &str = "pair-requested";
    /// 配对成立, payload: PeerDto
    pub const PAIRED: &str = "paired";
    /// 配对解除, payload: 指纹字符串
    pub const UNPAIRED: &str = "unpaired";
    /// 远端剪贴板已应用, payload: SyncedDto
    pub const CLIPBOARD_SYNCED: &str = "clipboard-synced";
    /// 同步开关变化(托盘切换回显设置窗), payload: bool
    pub const SYNC_STATE: &str = "sync-state-changed";
    /// 历史内容变化(新增/计数/删除/清空), 面板与设置页刷新用, 无载荷
    pub const HISTORY_CHANGED: &str = "history-changed";
    /// 无痕模式变化(托盘切换回显), payload: bool
    pub const INCOGNITO_STATE: &str = "incognito-changed";
}

/// 节点信息 DTO(peer-up / pair-requested / paired 事件与设备列表共用)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerDto {
    /// 设备 ID
    pub device_id: String,
    /// 展示名
    pub name: String,
    /// 证书指纹
    pub fingerprint: String,
    /// 平台标识
    pub platform: String,
    /// 系统版本描述
    pub os_version: Option<String>,
}

impl From<&PeerInfo> for PeerDto {
    fn from(info: &PeerInfo) -> Self {
        Self {
            device_id: info.device_id.clone(),
            name: info.name.clone(),
            fingerprint: info.fingerprint.clone(),
            platform: info.platform.clone(),
            os_version: info.os_version.clone(),
        }
    }
}

/// 远端同步事件 DTO
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncedDto {
    /// 来源设备名
    pub from_name: String,
    /// 内容预览(首行截断, 展示用)
    pub preview: String,
    /// 应用时刻(Unix 毫秒)
    pub at: u64,
}

/// 启动引擎并装配剪贴板与事件泵; setup 内经 block_on 同步等待,
/// 保证首个 command 到达时 AppState 已就绪
pub async fn start_engine(app: tauri::AppHandle, data_dir: PathBuf) -> anyhow::Result<AppState> {
    let settings = Settings::load(&data_dir);
    let settings_shared = Arc::new(Mutex::new(settings.clone()));

    let (clip_tx, clip_rx) = mpsc::channel(16);
    let (engine, events_rx) = SyncEngine::start(
        EngineConfig {
            data_dir: data_dir.clone(),
            tcp_port: settings.tcp_port,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            passive: false,
            sync_enabled: settings.sync_enabled,
        },
        clip_rx,
    )
    .await?;

    // 接管系统剪贴板监视: 本机复制 → 引擎(变化戳轮询, 决策 #4)
    clipboard::spawn_watcher(clip_tx);
    let engine = Arc::new(engine);
    let history = Arc::new(HistoryStore::load(&data_dir));
    let incognito = Arc::new(AtomicBool::new(false));
    let history_tx = spawn_history_worker(
        app.clone(),
        Arc::clone(&history),
        Arc::clone(&settings_shared),
    );
    spawn_event_pump(PumpDeps {
        app,
        events_rx,
        settings: Arc::clone(&settings_shared),
        engine: Arc::clone(&engine),
        history_tx,
        incognito: Arc::clone(&incognito),
    });

    Ok(AppState {
        engine,
        settings: settings_shared,
        history,
        incognito,
        data_dir,
    })
}

/// 事件泵的依赖集(全部经 Arc 捕获, 不经 app.state 以避开注入时序)
struct PumpDeps {
    app: tauri::AppHandle,
    events_rx: mpsc::Receiver<EngineEvent>,
    settings: Arc<Mutex<Settings>>,
    engine: Arc<SyncEngine>,
    history_tx: mpsc::Sender<HistoryJob>,
    incognito: Arc<AtomicBool>,
}

/// 历史记录任务(泵 → worker)
struct HistoryJob {
    content: ClipboardContent,
    hash: String,
    at: u64,
    origin: Option<String>,
}

/// 从设置摘取历史记录配置快照
fn history_config(settings: &Mutex<Settings>) -> HistoryConfig {
    let s = lock(settings);
    HistoryConfig {
        max_entries: s.history_max_entries,
        record_text: s.history_record_text,
        record_images: s.history_record_images,
        record_files: s.history_record_files,
    }
}

/// 历史记录专用串行 worker: 把 record(含大图 PNG 编码, 可达秒级)
/// 移出唯一事件泵, 防止队头阻塞 ApplyRemote/配对弹窗等实时事件;
/// 单任务顺序消费保证去重查插不并发。
fn spawn_history_worker(
    app: tauri::AppHandle,
    history: Arc<HistoryStore>,
    settings: Arc<Mutex<Settings>>,
) -> mpsc::Sender<HistoryJob> {
    let (tx, mut rx) = mpsc::channel::<HistoryJob>(32);
    tauri::async_runtime::spawn(async move {
        while let Some(job) = rx.recv().await {
            let outcome = history
                .record(
                    &job.content,
                    &job.hash,
                    job.at,
                    job.origin,
                    history_config(&settings),
                )
                .await;
            if outcome != crate::history::RecordOutcome::Skipped {
                emit(&app, events::HISTORY_CHANGED, ());
            }
        }
    });
    tx
}

/// 单路事件泵: EngineEvent → 剪贴板落地 / 历史记录 / 系统通知 / Tauri 事件
fn spawn_event_pump(deps: PumpDeps) {
    let PumpDeps {
        app,
        mut events_rx,
        settings,
        engine,
        history_tx,
        incognito,
    } = deps;
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            match event {
                EngineEvent::PeerUp(peer) => {
                    emit(&app, events::PEER_UP, PeerDto::from(&peer.info));
                }
                EngineEvent::PeerDown(fingerprint) => {
                    emit(&app, events::PEER_DOWN, fingerprint);
                }
                EngineEvent::PairRequested { peer } => {
                    let texts =
                        locale::texts(locale::Lang::from_settings(&lock(&settings).language));
                    notify_if_unfocused(
                        &app,
                        &texts.pair_request(&peer.name),
                        texts.pair_request_body,
                    );
                    emit(&app, events::PAIR_REQUESTED, PeerDto::from(&peer));
                }
                EngineEvent::Paired { peer } => {
                    emit(&app, events::PAIRED, PeerDto::from(&peer));
                }
                EngineEvent::Unpaired { fingerprint } => {
                    emit(&app, events::UNPAIRED, fingerprint);
                }
                // 本机复制 → 历史 worker(方案 14 节; 无痕模式下暂停记录)
                EngineEvent::LocalCopied {
                    content,
                    hash,
                    timestamp_ms,
                } => {
                    if incognito.load(Ordering::Relaxed) {
                        continue;
                    }
                    let _ = history_tx
                        .send(HistoryJob {
                            content,
                            hash,
                            at: timestamp_ms,
                            origin: None,
                        })
                        .await;
                }
                EngineEvent::ApplyRemote {
                    text, from, hash, ..
                } => {
                    let preview = preview_of(&text);
                    // 装配层落地: 写系统剪贴板(回声哈希已在引擎侧登记);
                    // 失败必须撤销回声登记, 否则孤儿哈希会误吞下一次
                    // 同内容的真实本机复制(ApplyRemote 契约)
                    if let Err(e) = clipboard::write_text(text.clone()).await {
                        tracing::warn!("远端同步写入系统剪贴板失败: {e}");
                        engine.cancel_echo(&hash);
                        continue;
                    }
                    // 远端写入也计入历史(origin = 来源设备名, 方案 14.1);
                    // 回声事件会被引擎吞掉不经 LocalCopied, 此处是唯一入口
                    if !incognito.load(Ordering::Relaxed) {
                        let _ = history_tx
                            .send(HistoryJob {
                                content: ClipboardContent::Text(text),
                                hash,
                                at: now_ms(),
                                origin: Some(from.name.clone()),
                            })
                            .await;
                    }
                    let (notify, lang) = {
                        let s = lock(&settings);
                        (s.notify_on_sync, locale::Lang::from_settings(&s.language))
                    };
                    if notify {
                        let texts = locale::texts(lang);
                        notify_if_unfocused(&app, &texts.synced_from(&from.name), &preview);
                    }
                    emit(
                        &app,
                        events::CLIPBOARD_SYNCED,
                        SyncedDto {
                            from_name: from.name.clone(),
                            preview,
                            at: now_ms(),
                        },
                    );
                }
                EngineEvent::SyncSent { to, result } => match result {
                    Ok(()) => tracing::debug!(to = %to.name, "剪贴板已同步至对端"),
                    Err(e) => tracing::info!(to = %to.name, "同步失败: {e}"),
                },
            }
        }
    });
}

/// 发 Tauri 事件; 失败仅记日志, 不影响引擎
fn emit<T: Serialize + Clone>(app: &tauri::AppHandle, event: &str, payload: T) {
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!("前端事件 {event} 发送失败: {e}");
    }
}

/// 主窗口不在前台时发系统通知(聚焦时应用内已可见, 不打扰)
pub fn notify_if_unfocused(app: &tauri::AppHandle, title: &str, body: &str) {
    use tauri::Manager;
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if focused {
        return;
    }
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::debug!("系统通知发送失败: {e}");
    }
}

/// 文本预览: 首行截前 60 字符(仅展示, 不改原文)
fn preview_of(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default();
    let preview: String = first_line.chars().take(60).collect();
    if preview.len() < text.len() {
        format!("{preview}…")
    } else {
        preview
    }
}
