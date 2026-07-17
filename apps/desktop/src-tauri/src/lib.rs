//! lanecho 桌面端应用壳: 插件装配、生命周期与托盘
//!
//! 关键约定(deskmate 实战继承):
//! - 关窗 = prevent_close + hide(托盘常驻), 真退出只走托盘菜单
//! - Ctrl-C/SIGTERM 经 wait_for_termination 引到 handle.exit(0),
//!   保证 RunEvent::Exit 里引擎优雅关闭(goodbye + mDNS 注销)能执行
//! - 托盘菜单文案创建时固定, 语言/开关状态变化需整菜单重建

mod bridge;
mod commands;
mod history;
mod locale;
mod settings;
mod state;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use state::{AppState, lock};

/// 托盘 ID(重建菜单时按此查找)
const TRAY_ID: &str = "main-tray";

/// 最近一次唤起历史面板的时刻(Unix 毫秒)
///
/// macOS 在"应用被激活且当时无可见窗口"时会发 RunEvent::Reopen;
/// 面板唤起经 set_focus 激活应用恰好命中该条件, 若不加区分,
/// Reopen 处理会把主窗静默 show 在 alwaysOnTop 面板底下 ——
/// 面板一关设置窗突然露出(真实踩过)。以时间窗区分"面板唤起的
/// 副产物"与"用户真点 Dock"。
static LAST_PANEL_SHOW_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 应用入口
pub fn run() {
    init_logging();
    let app = tauri::Builder::default()
        // 单实例必须最先注册: 二次启动进程直接退出, 由首实例唤起窗口
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        // 全局快捷键: 面板唤起 + 序号槽位直贴(方案 14.6, 动作全在 Rust 侧,
        // 面板未开也生效); 具体绑定在 setup 的 apply_hotkeys 里注册
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        return;
                    }
                    handle_shortcut(app, shortcut);
                })
                .build(),
        )
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            // 同步等引擎起好: 首个 command 到达时 state 必须已 manage
            let state = tauri::async_runtime::block_on(bridge::start_engine(
                app.handle().clone(),
                data_dir,
            ))?;
            app.manage(state);
            setup_tray(app.handle())?;
            // 注册全局快捷键(失败不致命: 托盘仍可达, 设置页可改绑)
            let hotkey_settings = lock(&app.state::<AppState>().settings).clone();
            if let Err(e) = apply_hotkeys(app.handle(), &hotkey_settings) {
                tracing::warn!("全局快捷键注册失败(可在设置中改绑): {e}");
            }

            // Ctrl-C/SIGTERM 引到正常退出路径(否则 goodbye 发不出)
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                wait_for_termination().await;
                handle.exit(0);
            });

            // 自启实例: 隐入托盘(与 autostart 注册的 --hidden 参数字面量一致)
            if std::env::args().any(|a| a == "--hidden")
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                // 关窗 = 隐藏(托盘常驻): 必须先 prevent_close 否则应用真退出
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // 历史浮窗: 失焦即隐(Maccy 形态, 方案 14.5)
                WindowEvent::Focused(false) if window.label() == "panel" => {
                    let _ = window.hide();
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::set_display_name,
            commands::get_self_info,
            commands::list_devices,
            commands::pair_device,
            commands::respond_pair,
            commands::unpair_device,
            commands::list_history,
            commands::copy_history_entry,
            commands::delete_history_entry,
            commands::clear_history,
            commands::pin_history_entry,
            commands::history_usage,
            commands::set_incognito,
            commands::get_incognito,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用构建失败");

    app.run(|app_handle, event| match event {
        // macOS Dock 点击只发 Reopen, 不能靠 window event;
        // 刚唤起过面板时的 Reopen 是激活副产物(见 LAST_PANEL_SHOW_MS), 忽略
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => {
            let since_panel = lanecho_core::clipboard::now_ms()
                .saturating_sub(LAST_PANEL_SHOW_MS.load(std::sync::atomic::Ordering::Relaxed));
            if since_panel > 1500 {
                show_main_window(app_handle);
            }
        }
        // 真退出: 引擎优雅关闭(goodbye + mDNS 注销, 对端即时感知下线)
        RunEvent::Exit => {
            let state = app_handle.state::<AppState>();
            tauri::async_runtime::block_on(state.engine.shutdown());
        }
        _ => {}
    });
}

/// 初始化 tracing 日志: 输出到 stderr, 级别由 RUST_LOG 控制(默认 info)
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// 等待终止信号: unix 下 SIGTERM + Ctrl-C, 其余平台仅 Ctrl-C
async fn wait_for_termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// 唤起主窗口(托盘/单实例/Dock 共用)
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 唤起历史浮窗: 定位到光标附近后显示聚焦(方案 14.5)
///
/// 必须对光标所在显示器做边缘 clamp: 托盘图标恰在屏幕角落(macOS 右上 /
/// Windows 右下), 直接把面板左上角放光标处会整体溢出屏外 —— OS 不会把
/// 应用主动 set_position 的窗口拉回屏内。
fn show_panel(app: &tauri::AppHandle) {
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };
    LAST_PANEL_SHOW_MS.store(
        lanecho_core::clipboard::now_ms(),
        std::sync::atomic::Ordering::Relaxed,
    );
    if let Ok(pos) = app.cursor_position() {
        let (mut x, mut y) = (pos.x, pos.y);
        let panel_size = panel
            .outer_size()
            .unwrap_or(tauri::PhysicalSize::new(380, 480));
        if let Ok(Some(monitor)) = app.monitor_from_point(pos.x, pos.y) {
            let mon_pos = monitor.position();
            let mon_size = monitor.size();
            let max_x =
                f64::from(mon_pos.x) + f64::from(mon_size.width) - f64::from(panel_size.width);
            let max_y =
                f64::from(mon_pos.y) + f64::from(mon_size.height) - f64::from(panel_size.height);
            x = x.min(max_x).max(f64::from(mon_pos.x));
            y = y.min(max_y).max(f64::from(mon_pos.y));
        }
        let _ = panel.set_position(tauri::PhysicalPosition::new(x, y));
    }
    let _ = panel.show();
    let _ = panel.set_focus();
}

/// 按设置注册全局快捷键(先清空再注册; 供启动与设置变更共用)
///
/// 面板键解析/注册失败返回 Err(设置页以 hotkey_invalid 反馈);
/// 槽位键(Alt+1..6)被其他应用占用时逐个告警跳过, 不整体失败。
pub fn apply_hotkeys(app: &tauri::AppHandle, settings: &settings::Settings) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    if !settings.panel_hotkey.is_empty() {
        let shortcut: Shortcut = settings
            .panel_hotkey
            .parse()
            .map_err(|e| format!("{e:?}"))?;
        shortcuts.register(shortcut).map_err(|e| e.to_string())?;
    }
    if settings.slot_hotkeys {
        for n in 1..=6u8 {
            let parsed: Result<Shortcut, _> = format!("Alt+{n}").parse();
            match parsed {
                Ok(shortcut) => {
                    if let Err(e) = shortcuts.register(shortcut) {
                        tracing::warn!("槽位快捷键 Alt+{n} 注册失败(可能被占用): {e}");
                    }
                }
                Err(e) => tracing::warn!("槽位快捷键 Alt+{n} 解析失败: {e:?}"),
            }
        }
    }
    Ok(())
}

/// 全局快捷键分发: 面板唤起 / 槽位直贴
fn handle_shortcut(app: &tauri::AppHandle, shortcut: &tauri_plugin_global_shortcut::Shortcut) {
    use tauri_plugin_global_shortcut::Shortcut;

    let state = app.state::<AppState>();
    let (panel_hotkey, sort) = {
        let s = lock(&state.settings);
        (s.panel_hotkey.clone(), s.history_sort.clone())
    };
    if let Ok(panel_shortcut) = panel_hotkey.parse::<Shortcut>()
        && *shortcut == panel_shortcut
    {
        show_panel(app);
        return;
    }
    for n in 1..=6u8 {
        if let Ok(slot) = format!("Alt+{n}").parse::<Shortcut>()
            && *shortcut == slot
        {
            // 槽位 N = 面板当前排序下第 N 条(方案 14.4)
            let Some(entry) = state
                .history
                .list(&sort)
                .into_iter()
                .nth(usize::from(n) - 1)
            else {
                return;
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = commands::copy_entry_to_clipboard(&app, &entry.id).await {
                    tracing::warn!("槽位直贴失败: {e}");
                }
            });
            return;
        }
    }
}

/// 构建托盘菜单(语言与勾选状态在创建时固定, 变化时整体重建)
fn build_tray_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let texts = locale::current(app);
    let state = app.state::<AppState>();
    let sync_enabled = lock(&state.settings).sync_enabled;
    let incognito = state.incognito.load(std::sync::atomic::Ordering::Relaxed);
    let sync = CheckMenuItem::with_id(
        app,
        "toggle_sync",
        texts.tray_sync,
        true,
        sync_enabled,
        None::<&str>,
    )?;
    let pause = CheckMenuItem::with_id(
        app,
        "incognito",
        texts.tray_incognito,
        true,
        incognito,
        None::<&str>,
    )?;
    let history = MenuItem::with_id(app, "history", texts.tray_history, true, None::<&str>)?;
    let open = MenuItem::with_id(app, "settings", texts.tray_settings, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", texts.tray_quit, true, None::<&str>)?;
    Menu::with_items(app, &[&sync, &pause, &history, &open, &quit])
}

/// 重建托盘菜单(语言切换 / 同步开关变化后调用)
pub fn refresh_tray_menu(app: &tauri::AppHandle) {
    let result = build_tray_menu(app).and_then(|menu| match app.tray_by_id(TRAY_ID) {
        Some(tray) => tray.set_menu(Some(menu)),
        None => Ok(()),
    });
    if let Err(e) = result {
        tracing::warn!("托盘菜单重建失败: {e}");
    }
}

/// 托盘菜单里切换同步开关(与设置窗共享 settings.sync_enabled)
///
/// 与 save_settings 同模式: 先落盘成功再施加副作用, 失败则整体不生效
/// (托盘无错误展示渠道, 保持旧态比半应用分裂更可取)。
fn toggle_sync_from_tray(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let mut next = lock(&state.settings).clone();
    next.sync_enabled = !next.sync_enabled;
    if let Err(e) = next.save(&state.data_dir) {
        tracing::warn!("同步开关持久化失败, 本次切换不生效: {e}");
        refresh_tray_menu(app); // 恢复勾选显示为实际状态
        return;
    }
    let enabled = next.sync_enabled;
    *lock(&state.settings) = next;
    state.engine.set_sync_enabled(enabled);
    refresh_tray_menu(app);
    let _ = app.emit(bridge::events::SYNC_STATE, enabled);
}

/// 创建系统托盘: 左键唤窗, 右键菜单(同步开关 / 打开设置 / 退出)
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        // 左键唤窗、右键弹菜单(不设则左键弹菜单)
        .show_menu_on_left_click(false)
        .tooltip("lanecho")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle_sync" => toggle_sync_from_tray(app),
            "incognito" => {
                let state = app.state::<AppState>();
                let next = !state
                    .incognito
                    .load(std::sync::atomic::Ordering::Relaxed);
                state
                    .incognito
                    .store(next, std::sync::atomic::Ordering::Relaxed);
                refresh_tray_menu(app);
                let _ = app.emit(bridge::events::INCOGNITO_STATE, next);
            }
            "history" => show_panel(app),
            "settings" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键 = 历史面板(方案 14.5: 面板是主交互面); 菜单走右键
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_panel(tray.app_handle());
            }
        });

    // macOS 用单色模板图标(随系统明暗自动着色); 解码失败回退应用图标
    #[cfg(target_os = "macos")]
    {
        match tauri::image::Image::from_bytes(include_bytes!("../icons/tray-iconTemplate.png")) {
            Ok(template) => {
                tray = tray.icon(template).icon_as_template(true);
            }
            Err(e) => {
                tracing::warn!("托盘模板图标解码失败, 回退应用图标: {e}");
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(icon) = app.default_window_icon() {
            tray = tray.icon(icon.clone());
        }
    }

    tray.build(app)?;
    Ok(())
}
