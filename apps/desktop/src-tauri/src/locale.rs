//! 桌面壳用户可见文案(托盘菜单 + 系统通知)的双语文案表
//!
//! 前端界面文案在 apps/desktop/src/i18n/(zh.ts / en.ts), 与本文件分开维护。
//! 语言取自设置(settings.language, 由前端首启按系统语言检测写入);
//! 设置尚未初始化时退化读 LANG 环境变量(macOS GUI 进程常缺失 → 英文,
//! 前端初始化后写回设置并热更新托盘, 只影响首启头几秒)。

use tauri::Manager;

use crate::state::{AppState, lock};

/// 支持的界面语言
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 中文
    Zh,
    /// 英文
    En,
}

impl Lang {
    /// 从设置值解析; 未初始化(空/未知)时按环境变量兜底
    pub fn from_settings(value: &str) -> Self {
        match value {
            "zh" => Lang::Zh,
            "en" => Lang::En,
            _ => Self::system_fallback(),
        }
    }

    /// 环境变量兜底判断(仅设置未初始化时使用)
    fn system_fallback() -> Self {
        let lang_env = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        if lang_env.to_lowercase().starts_with("zh") {
            Lang::Zh
        } else {
            Lang::En
        }
    }
}

/// 桌面壳文案表(带 {name} 占位的字段经辅助方法填充)
pub struct ShellTexts {
    /// 托盘菜单: 同步开关
    pub tray_sync: &'static str,
    /// 托盘菜单: 暂停记录(无痕模式)
    pub tray_incognito: &'static str,
    /// 托盘菜单: 历史面板
    pub tray_history: &'static str,
    /// 托盘菜单: 打开设置
    pub tray_settings: &'static str,
    /// 托盘菜单: 退出
    pub tray_quit: &'static str,
    /// 通知标题模板: 配对请求({name} = 对方昵称)
    pair_request: &'static str,
    /// 通知正文: 配对请求的操作提示
    pub pair_request_body: &'static str,
    /// 通知标题模板: 已从对端同步({name} = 来源昵称)
    synced_from: &'static str,
}

impl ShellTexts {
    /// 组装"配对请求"通知标题
    pub fn pair_request(&self, name: &str) -> String {
        self.pair_request.replace("{name}", name)
    }

    /// 组装"已从 X 同步"通知标题
    pub fn synced_from(&self, name: &str) -> String {
        self.synced_from.replace("{name}", name)
    }
}

/// 中文文案
const ZH: ShellTexts = ShellTexts {
    tray_sync: "剪贴板同步",
    tray_incognito: "暂停记录",
    tray_history: "历史面板",
    tray_settings: "打开 lanecho",
    tray_quit: "退出",
    pair_request: "{name} 请求配对",
    pair_request_body: "打开 lanecho 接受或拒绝",
    synced_from: "已从 {name} 同步",
};

/// 英文文案
const EN: ShellTexts = ShellTexts {
    tray_sync: "Clipboard sync",
    tray_incognito: "Pause recording",
    tray_history: "History panel",
    tray_settings: "Open lanecho",
    tray_quit: "Quit",
    pair_request: "Pairing request from {name}",
    pair_request_body: "Open lanecho to accept or decline",
    synced_from: "Synced from {name}",
};

/// 按语言取文案表
pub fn texts(lang: Lang) -> &'static ShellTexts {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

/// 按当前设置取文案表(托盘/通知发送时实时调用, 语言切换即时生效)
pub fn current(app: &tauri::AppHandle) -> &'static ShellTexts {
    let lang = Lang::from_settings(&lock(&app.state::<AppState>().settings).language);
    texts(lang)
}
