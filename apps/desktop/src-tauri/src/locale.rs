//! Bilingual text table for the desktop shell's user-visible strings: the
//! tray menu (kept on Linux only) and system notifications.
//!
//! Frontend UI strings live in apps/desktop/src/i18n/ (zh.ts / en.ts) and are
//! maintained separately from this file. The language comes from the settings
//! (settings.language, written by the frontend on first launch after it
//! detects the system language); before the settings are initialized it falls
//! back to the LANG environment variable (often absent in macOS GUI processes,
//! hence English). Once the frontend writes the setting back the tray
//! hot-updates, so this only affects the first few seconds of a first launch.

use tauri::Manager;

use crate::state::{AppState, lock};

/// Supported UI languages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Chinese
    Zh,
    /// English
    En,
}

impl Lang {
    /// Parse from the settings value; falls back to the environment variable
    /// when uninitialized (empty or unknown)
    pub fn from_settings(value: &str) -> Self {
        match value {
            "zh" => Lang::Zh,
            "en" => Lang::En,
            _ => Self::system_fallback(),
        }
    }

    /// Environment variable fallback, used only when the settings are
    /// uninitialized
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

/// Desktop shell text table; fields carrying a {name} placeholder are filled
/// in by the helper methods
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "tray_* fields are read only by the Linux tray menu; other platforms moved the menu to the panel footer, but Linux CI still validates all fields"
    )
)]
pub struct ShellTexts {
    /// Tray menu: sync toggle
    pub tray_sync: &'static str,
    /// Tray menu: history panel
    pub tray_history: &'static str,
    /// Tray menu: open settings
    pub tray_settings: &'static str,
    /// Tray: about
    pub tray_about: &'static str,
    /// Tray menu: quit
    pub tray_quit: &'static str,
    /// Notification title template: pairing request ({name} = peer's name)
    pair_request: &'static str,
    /// Notification body: what to do about a pairing request
    pub pair_request_body: &'static str,
    /// Notification title template: synced from a peer ({name} = source name)
    synced_from: &'static str,
    /// The word for "image" in a notification preview, used when a remote
    /// image arrives
    pub image_word: &'static str,
    /// Tray tooltip template: pending pairing requests ({n} = count)
    tray_pending: &'static str,
    /// Startup guard dialog title, shown when the engine cannot start
    pub start_failed_title: &'static str,
    /// Troubleshooting hint in the startup guard dialog; the typical cause is
    /// a port conflict with another client on the same machine
    pub start_failed_hint: &'static str,
}

impl ShellTexts {
    /// Build the "pairing request" notification title
    pub fn pair_request(&self, name: &str) -> String {
        self.pair_request.replace("{name}", name)
    }

    /// Build the "synced from X" notification title
    pub fn synced_from(&self, name: &str) -> String {
        self.synced_from.replace("{name}", name)
    }

    /// Build the "N pairing request(s) pending" tray tooltip
    pub fn tray_pending(&self, n: usize) -> String {
        self.tray_pending.replace("{n}", &n.to_string())
    }
}

/// Chinese strings
const ZH: ShellTexts = ShellTexts {
    tray_sync: "剪贴板同步",
    tray_history: "历史面板",
    tray_settings: "偏好设置…",
    tray_about: "关于",
    tray_quit: "退出",
    pair_request: "{name} 请求配对",
    pair_request_body: "打开 Lanecho 接受或拒绝",
    synced_from: "已从 {name} 同步",
    image_word: "图像",
    tray_pending: "Lanecho — {n} 个配对请求待处理",
    start_failed_title: "Lanecho 启动失败",
    start_failed_hint: "若同机还开着另一个 Lanecho 客户端(端口冲突): \
先退出对方, 启动本应用改掉端口, 之后即可同时运行。",
};

/// English strings
const EN: ShellTexts = ShellTexts {
    tray_sync: "Clipboard sync",
    tray_history: "History panel",
    tray_settings: "Preferences…",
    tray_about: "About",
    tray_quit: "Quit",
    pair_request: "Pairing request from {name}",
    pair_request_body: "Open Lanecho to accept or decline",
    synced_from: "Synced from {name}",
    image_word: "Image",
    tray_pending: "Lanecho — {n} pairing request(s) pending",
    start_failed_title: "Lanecho failed to start",
    start_failed_hint: "If another Lanecho client is running on this machine \
(port conflict): quit it, start this app to change the port, then run both.",
};

/// Get the text table for a language
pub fn texts(lang: Lang) -> &'static ShellTexts {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

/// Get the text table for the current setting; called live whenever a tray
/// update or notification is sent, so a language switch applies immediately
pub fn current(app: &tauri::AppHandle) -> &'static ShellTexts {
    let lang = Lang::from_settings(&lock(&app.state::<AppState>().settings).language);
    texts(lang)
}
