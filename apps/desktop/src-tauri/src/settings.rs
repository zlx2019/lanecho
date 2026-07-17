//! 应用设置: settings.json 持久化于数据目录
//!
//! 生效时机: 仅监听端口重启后生效(socket 启动时固定), 其余各项即时生效。
//! 昵称不在此处 —— identity.json 是展示名唯一真源(经 set_display_name 命令走
//! 引擎热更新), 避免双持久层漂移。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 设置文件名
const SETTINGS_FILE: &str = "settings.json";

/// 用户设置
///
/// `rename_all = camelCase` 与前端 DTO 对齐; `default` 让老 JSON 缺字段
/// 自动补默认值(加字段向后兼容的关键, deskmate 约定继承)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// TCP 监听端口(0 表示随机; 重启后生效)
    pub tcp_port: u16,
    /// 开机自启(保存设置时即时写入系统)
    pub autostart: bool,
    /// 同步开关(熔断闸; 托盘菜单与设置窗共同维护, 持久化)
    pub sync_enabled: bool,
    /// 剪贴板被远端覆盖时弹系统通知("已从 X 同步", 即时生效)
    pub notify_on_sync: bool,
    /// 界面语言: "zh" / "en"; 空表示未初始化(首启由前端按系统语言检测后写入)
    pub language: String,
    /// 历史保留条目上限(超限按"未固定 + 最旧"淘汰)
    pub history_max_entries: usize,
    /// 历史记录类型开关: 文本
    pub history_record_text: bool,
    /// 历史记录类型开关: 图像
    pub history_record_images: bool,
    /// 历史记录类型开关: 文件引用
    pub history_record_files: bool,
    /// 历史排序: "recent"(最近优先)/ "frequent"(次数优先)
    pub history_sort: String,
    /// 历史面板唤起快捷键(Tauri 快捷键语法; 空串 = 禁用)
    pub panel_hotkey: String,
    /// 序号槽位直贴(Alt+1..6)开关
    pub slot_hotkeys: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tcp_port: lanecho_core::DEFAULT_TCP_PORT,
            autostart: false,
            sync_enabled: true,
            notify_on_sync: true,
            language: String::new(),
            history_max_entries: 200,
            history_record_text: true,
            history_record_images: true,
            history_record_files: true,
            history_sort: "recent".to_string(),
            panel_hotkey: "CmdOrCtrl+Shift+V".to_string(),
            slot_hotkeys: true,
        }
    }
}

impl Settings {
    /// 从数据目录加载; 文件缺失或损坏时回退默认值
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// 持久化到数据目录(原子写: 直接覆盖被中断会留下半截 JSON,
    /// 下次 load 静默回默认值 —— 快捷键/开关/语言全部无声重置)
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = data_dir.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, data_dir.join(SETTINGS_FILE))
    }
}
