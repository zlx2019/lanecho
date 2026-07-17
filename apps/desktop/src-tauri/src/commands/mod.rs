//! Tauri 命令层: 前端 invoke 的入口
//!
//! 错误统一 [`ErrDto`]{ code, detail }: 前端按 code 查 i18n errors 分区渲染,
//! detail 不译原样附加(deskmate 错误码模式继承)。

use serde::Serialize;
use tauri::{Manager, State};

use lanecho_core::sync::SyncError;

use crate::bridge::events;
use crate::settings::Settings;
use crate::state::{AppState, lock};

/// 结构化错误 DTO
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrDto {
    /// 稳定错误码(前端 i18n errors 分区的键)
    pub code: &'static str,
    /// 原始细节(不译, 展示层原样附加)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl std::fmt::Display for ErrDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{}: {detail}", self.code),
            None => write!(f, "{}", self.code),
        }
    }
}

impl ErrDto {
    /// 仅错误码
    pub fn new(code: &'static str) -> Self {
        Self { code, detail: None }
    }

    /// 错误码 + 细节
    pub fn with(code: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            code,
            detail: Some(detail.to_string()),
        }
    }
}

impl From<&SyncError> for ErrDto {
    fn from(err: &SyncError) -> Self {
        match err {
            SyncError::PeerUnreachable => ErrDto::new("peer_unreachable"),
            SyncError::PairRejected => ErrDto::new("pair_rejected"),
            SyncError::FingerprintMismatch => ErrDto::new("fingerprint_mismatch"),
            SyncError::Timeout(step) => ErrDto::with("timeout", step),
            // 对端结构化拒因码直接透传(not_paired / too_large / disabled / unsupported_type)
            SyncError::Rejected(code) => match code.as_str() {
                "not_paired" => ErrDto::new("not_paired"),
                "too_large" => ErrDto::new("too_large"),
                "disabled" => ErrDto::new("disabled"),
                "unsupported_type" => ErrDto::new("unsupported_type"),
                other => ErrDto::with("rejected", other),
            },
            other => ErrDto::with("engine", other),
        }
    }
}

/// 本机信息 DTO
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfInfoDto {
    /// 展示名
    pub name: String,
    /// 设备 ID
    pub device_id: String,
    /// 证书指纹
    pub fingerprint: String,
    /// 平台标识
    pub platform: String,
    /// 实际监听端口
    pub port: u16,
}

/// 设备列表条目: 在线节点与已配对(可能离线)设备的合并视图
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    /// 展示名(在线取广播名, 离线取配对时快照名)
    pub name: String,
    /// 证书指纹(唯一键)
    pub fingerprint: String,
    /// 平台标识(仅在线时已知)
    pub platform: Option<String>,
    /// 系统版本描述(仅在线时已知)
    pub os_version: Option<String>,
    /// 当前是否在线
    pub online: bool,
    /// 是否已配对
    pub paired: bool,
}

/// 读取设置
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    lock(&state.settings).clone()
}

/// 保存设置: **先落盘再施加副作用**(deskmate prefs 模式)——
/// 落盘失败时引擎/内存/托盘均保持旧态, 不产生"半应用"的状态分裂
#[tauri::command]
pub fn save_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<(), ErrDto> {
    let old = lock(&state.settings).clone();
    let language_changed = old.language != settings.language;
    let sync_changed = old.sync_enabled != settings.sync_enabled;

    // 可失败步骤最先: 失败即整体失败, 无任何已应用的副作用
    settings
        .save(&state.data_dir)
        .map_err(|e| ErrDto::with("settings_save_failed", e))?;
    *lock(&state.settings) = settings.clone();

    // 以下均为幂等副作用, 按新值同步
    if old.autostart != settings.autostart {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        let result = if settings.autostart {
            launcher.enable()
        } else {
            launcher.disable()
        };
        if let Err(e) = result {
            tracing::warn!("开机自启设置失败: {e}");
        }
    }
    if sync_changed {
        state.engine.set_sync_enabled(settings.sync_enabled);
        use tauri::Emitter;
        let _ = app.emit(events::SYNC_STATE, settings.sync_enabled);
    }
    // 托盘菜单文案/勾选在创建时固定, 语言或开关变化都整菜单重建
    if language_changed || sync_changed {
        crate::refresh_tray_menu(&app);
    }
    Ok(())
}

/// 热更新展示名(None/空串 = 恢复跟随 hostname); identity.json 为唯一真源
#[tauri::command]
pub fn set_display_name(state: State<'_, AppState>, name: Option<String>) -> Result<(), ErrDto> {
    let name = name.filter(|n| !n.trim().is_empty());
    state
        .engine
        .set_display_name(name.as_deref())
        .map_err(|e| ErrDto::with("rename_failed", e))
}

/// 本机身份信息
#[tauri::command]
pub fn get_self_info(state: State<'_, AppState>) -> SelfInfoDto {
    let info = state.engine.local_info();
    SelfInfoDto {
        name: info.name,
        device_id: info.device_id,
        fingerprint: info.fingerprint,
        platform: info.platform,
        port: state.engine.port(),
    }
}

/// 设备列表: 在线节点 ∪ 已配对设备(离线的置灰展示)
#[tauri::command]
pub fn list_devices(state: State<'_, AppState>) -> Vec<DeviceDto> {
    let peers = state.engine.peers();
    let paired = state.engine.paired_list();
    let mut devices: Vec<DeviceDto> = peers
        .iter()
        .map(|p| DeviceDto {
            name: p.info.name.clone(),
            fingerprint: p.info.fingerprint.clone(),
            platform: Some(p.info.platform.clone()),
            os_version: p.info.os_version.clone(),
            online: true,
            paired: paired.iter().any(|d| d.fingerprint == p.info.fingerprint),
        })
        .collect();
    for record in paired {
        if !devices.iter().any(|d| d.fingerprint == record.fingerprint) {
            devices.push(DeviceDto {
                name: record.name,
                fingerprint: record.fingerprint,
                platform: None,
                os_version: None,
                online: false,
                paired: true,
            });
        }
    }
    // 稳定排序: 在线优先, 同组按名称
    devices.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));
    devices
}

/// 向指定设备发起配对(等待对端用户确认, 长时 async)
#[tauri::command]
pub async fn pair_device(app: tauri::AppHandle, fingerprint: String) -> Result<(), ErrDto> {
    let state = app.state::<AppState>();
    state
        .engine
        .pair(&fingerprint)
        .await
        .map_err(|e| ErrDto::from(&e))
}

/// 回应入站配对请求(对应 pair-requested 事件)
#[tauri::command]
pub fn respond_pair(state: State<'_, AppState>, fingerprint: String, accept: bool) {
    state.engine.respond_pair(&fingerprint, accept);
}

/// 解除配对(本地立即生效, 尽力通知对端)
#[tauri::command]
pub async fn unpair_device(app: tauri::AppHandle, fingerprint: String) -> Result<(), ErrDto> {
    let state = app.state::<AppState>();
    state.engine.unpair(&fingerprint).await;
    Ok(())
}
