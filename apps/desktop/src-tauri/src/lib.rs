//! lanecho 桌面端应用壳: 插件装配、生命周期与托盘
//!
//! 关键约定(deskmate 实战继承):
//! - 关窗 = prevent_close + hide(托盘常驻), 真退出只走托盘菜单
//! - Ctrl-C/SIGTERM 经 wait_for_termination 引到 handle.exit(0),
//!   保证 RunEvent::Exit 里引擎优雅关闭(goodbye + mDNS 注销)能执行
//! - 托盘菜单文案创建时固定, 语言/开关状态变化需整菜单重建

mod bridge;
mod commands;
mod locale;
mod settings;
mod state;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use state::{AppState, lock};

/// 托盘 ID(重建菜单时按此查找)
const TRAY_ID: &str = "main-tray";

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
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            // 同步等引擎起好: 首个 command 到达时 state 必须已 manage
            let state = tauri::async_runtime::block_on(bridge::start_engine(
                app.handle().clone(),
                data_dir,
            ))?;
            app.manage(state);
            setup_tray(app.handle())?;

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
            // 关窗 = 隐藏(托盘常驻): 必须先 prevent_close 否则应用真退出
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
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
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用构建失败");

    app.run(|app_handle, event| match event {
        // macOS Dock 点击只发 Reopen, 不能靠 window event
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main_window(app_handle),
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

/// 构建托盘菜单(语言与同步开关状态在创建时固定, 变化时整体重建)
fn build_tray_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let texts = locale::current(app);
    let sync_enabled = lock(&app.state::<AppState>().settings).sync_enabled;
    let sync = CheckMenuItem::with_id(
        app,
        "toggle_sync",
        texts.tray_sync,
        true,
        sync_enabled,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(app, "settings", texts.tray_settings, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", texts.tray_quit, true, None::<&str>)?;
    Menu::with_items(app, &[&sync, &open, &quit])
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
            "settings" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
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
