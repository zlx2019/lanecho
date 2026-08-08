//! lanecho desktop application shell: plugin wiring, lifecycle and tray.
//!
//! Key rules:
//! - Closing a window = prevent_close + hide (the tray stays resident); a
//!   real quit only goes through the tray menu
//! - Ctrl-C/SIGTERM are routed to handle.exit(0) through
//!   wait_for_termination, so the engine's graceful shutdown (goodbye + mDNS
//!   deregistration) still runs inside RunEvent::Exit
//! - The tray menu is **kept on Linux only** (elsewhere its items moved to
//!   the bottom of the panel and both tray buttons open the panel); the Linux
//!   menu is updated **in place** through its handles (set_text /
//!   set_checked, see TrayMenu): replacing it wholesale with set_menu has no
//!   effect, do not go back to rebuilding it

mod autopaste;
mod bridge;
mod commands;
mod history;
mod ignore;
mod locale;
mod settings;
mod state;

#[cfg(target_os = "linux")]
use tauri::Emitter;
#[cfg(target_os = "linux")]
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WindowEvent};

use state::{AppState, lock};

/// Tray ID (used to look the tray up when updating the tooltip)
const TRAY_ID: &str = "main-tray";

/// When the panel was last hidden because it lost focus (Unix milliseconds)
///
/// A tray click runs in this order: the press steals focus → the panel hides
/// on blur → Click(Up) shows it again. Without a guard, a user trying to
/// dismiss the panel from the tray only sees it blink and come back. When
/// show_panel finds a blur-hide from the last 300ms it treats the trigger as
/// the closing half of a toggle and returns.
static LAST_PANEL_BLUR_HIDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Application entry point
pub fn run() {
    init_logging();
    let app = tauri::Builder::default()
        // single-instance must be registered first: a second launch exits
        // immediately and the first instance raises the window
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_notification::init())
        // Native dialogs (called from Rust only, for the startup guard; not
        // exposed to the frontend capability)
        .plugin(tauri_plugin_dialog::init())
        // External link to the project home page from the about window
        // (allowed by URL whitelist in capabilities)
        .plugin(tauri_plugin_opener::init())
        // The --hidden launch argument is kept: the settings window is never
        // shown at startup anyway (see setup), so the flag no longer makes
        // any behavioural difference here and only remains as the usual
        // login-item convention
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        // Global hotkeys: open the panel + paste a numbered slot directly
        // (the actions all live on the Rust side and work with the panel
        // closed); the actual bindings are registered by apply_hotkeys in
        // setup
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
            // Wait synchronously for the engine to come up: state must be
            // managed before the first command arrives. A failure goes
            // through the startup guard rather than `?`: an Err out of setup
            // makes Tauri panic inside did_finish_launching, which aborts
            // across the FFI boundary — all the user sees is an "unexpected
            // quit" crash report, with no way to tell the port is taken
            let state = match tauri::async_runtime::block_on(bridge::start_engine(
                app.handle().clone(),
                data_dir.clone(),
            )) {
                Ok(state) => state,
                Err(e) => {
                    startup_guard(app.handle(), &data_dir, &e);
                    return Ok(());
                }
            };
            app.manage(state);
            setup_tray(app.handle())?;
            // The preview card is a pure presentation layer by default: mouse
            // events pass through it, so it never competes for clicks or
            // focus (focusable=false is declared in the window config; cursor
            // pass-through has no config key, so it is set here).
            // show_preview_impl temporarily takes pass-through back when the
            // content exceeds the height cap, so the wheel can scroll it
            if let Some(preview) = app.get_webview_window("preview") {
                let _ = preview.set_ignore_cursor_events(true);
            }
            // Windows: the DWM drop shadow of an undecorated window is drawn
            // in frame insets around the client area that the webview never
            // paints — on a transparent window they render as black corners,
            // so the shadow has to go (macOS keeps its native NSWindow shadow)
            #[cfg(windows)]
            for label in ["panel", "preview"] {
                if let Some(window) = app.get_webview_window(label) {
                    let _ = window.set_shadow(false);
                }
            }
            // Park the panel before it is ever shown: the first open would
            // otherwise paint a frame at the placement the system chose when
            // the window was created (see PANEL_PARKING)
            park_panel(app.handle());
            // Register the global hotkeys (a failure is not fatal: the tray
            // is still reachable and the bindings can be changed in settings)
            let hotkey_settings = lock(&app.state::<AppState>().settings).clone();
            if let Err(e) = apply_hotkeys(app.handle(), &hotkey_settings) {
                tracing::warn!(
                    "Failed to register global hotkeys (bindings can be changed in settings): {e}"
                );
            }

            // Route Ctrl-C/SIGTERM into the normal exit path (otherwise
            // goodbye never goes out)
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                wait_for_termination().await;
                handle.exit(0);
            });

            // The main (settings) window is **not shown at startup**: the
            // entry points of a tray-resident application are the tray and
            // the panel, and popping the settings window on launch is an
            // interruption. The window config sets visible=false and nothing
            // here shows it — the user opens it from "Preferences…" at the
            // bottom of the panel, the Linux tray menu, or by launching the
            // application again (single-instance).
            //
            // The webview still loads while hidden: the settings page works
            // out its adaptive height up front, so the first open already
            // fits its content (when the layout cannot be measured the
            // frontend guards against the 0 value and recomputes once shown)
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                // Closing a window = hiding it (the tray stays resident):
                // prevent_close must come first or the application quits
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // The history panel hides on blur.
                // Only record and hide while it is visible: hide itself fires
                // another blur, which must not be timestamped again
                WindowEvent::Focused(false)
                    if window.label() == "panel" && window.is_visible().unwrap_or(false) =>
                {
                    LAST_PANEL_BLUR_HIDE_MS.store(
                        lanecho_core::clipboard::now_ms(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    hide_preview_impl(window.app_handle()); // dismiss the card too
                    let _ = window.hide();
                    park_panel(window.app_handle());
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
            commands::list_pending_pairs,
            commands::unpair_device,
            commands::list_history,
            commands::search_history,
            commands::history_entry_text,
            commands::hide_panel,
            commands::history_image_png,
            commands::app_icon_png,
            commands::show_preview,
            commands::hide_preview,
            commands::show_settings_window,
            commands::show_about,
            commands::app_version,
            commands::quit_app,
            commands::resize_settings_window,
            commands::copy_history_entry,
            commands::delete_history_entry,
            commands::clear_history,
            commands::pin_history_entry,
            commands::history_usage,
            commands::set_incognito,
            commands::get_incognito,
            commands::auto_paste_status,
            commands::request_auto_paste_permission,
            commands::window_effects_active,
            commands::get_slot_hotkey_failures,
            commands::pick_ignored_app,
        ])
        .build(tauri::generate_context!())
        .expect("Failed to build Tauri application");

    #[cfg(target_os = "macos")]
    let mut app = app;

    // Configure the tray-resident macOS process before the event loop starts.
    // Tao launches with Regular by default; changing the policy from setup()
    // is already too late and makes the first status-item interaction activate
    // the application as if it had a normal window.
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    // A real quit: shut the engine down gracefully (goodbye + mDNS
    // deregistration, so peers see us go offline at once); the history index
    // is flushed once synchronously — an async save dies with the process and
    // the last batch of copies would be lost
    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            // On the startup-guard path AppState was never managed, and an
            // unchecked lookup would panic on exit
            let Some(state) = app_handle.try_state::<AppState>() else {
                return;
            };
            // The engine goes down first: goodbye / mDNS deregistration go
            // out as early as possible (peers see us go offline at once) and
            // the pump stops producing new history jobs, so the drain really
            // drains. It only finishes in milliseconds because the pump holds
            // a WeakSender (see PumpDeps in bridge): the pump never exits (the
            // event sender lives inside SyncEngine), so with a strong sender
            // there the channel would never close and every exit would wait
            // out the full 3 second timeout
            tauri::async_runtime::block_on(state.engine.shutdown());
            drain_history_worker(&state);
            state.history.save_sync();
        }
    });
}

/// Startup guard: when the engine cannot come up (most typically both
/// clients running on the same machine, with the port held by the other
/// lanecho) it shows a readable error and exits cleanly once the user
/// confirms (matching the guard of the same name in the native client's
/// AppDelegate).
///
/// The callback form of `show` is mandatory: setup runs on the main thread
/// and the reply channel of `blocking_show` is serviced by the main thread,
/// so waiting for it here deadlocks against ourselves.
fn startup_guard(app: &tauri::AppHandle, data_dir: &std::path::Path, err: &anyhow::Error) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    // The top-level Display already carries the underlying cause (thiserror
    // embeds source), so the `:#` chained form would repeat it
    tracing::error!("Failed to start engine: {err}");
    // AppState does not exist yet, so read the language straight from disk
    // (one IO does not matter on the guard path)
    let lang = locale::Lang::from_settings(&settings::Settings::load(data_dir).language);
    let texts = locale::texts(lang);
    let handle = app.clone();
    app.dialog()
        .message(format!("{err}\n\n{}", texts.start_failed_hint))
        .title(texts.start_failed_title)
        .kind(MessageDialogKind::Error)
        .show(move |_| handle.exit(1));
}

/// Drains the history worker before exiting (must run before `save_sync`)
///
/// The image branch of `record` has an await between "the blob is on disk"
/// and "the entry is in the in-memory table": an index.json flushed at that
/// moment does not contain the entry while the blob already sits on disk —
/// the orphan sweep on the next launch deletes it, and from the user's point
/// of view the screenshot they just copied vanished. PNG encoding of a 5K
/// screenshot stretches that window from milliseconds to hundreds of
/// milliseconds, so quitting within a second of copying is enough to hit it.
///
/// With a timeout: a stuck drain must not keep the application from ever
/// exiting — force killing it would lose exactly the history that was about
/// to be written.
fn drain_history_worker(state: &AppState) {
    let Some(worker) = crate::state::lock(&state.history_worker).take() else {
        return;
    };
    // Close the sender first, so the worker sees the closed channel once the
    // queue runs dry and leaves its loop
    drop(worker.tx);
    tauri::async_runtime::block_on(async {
        if tokio::time::timeout(HISTORY_DRAIN_TIMEOUT, worker.handle)
            .await
            .is_err()
        {
            tracing::warn!("Timed out draining history worker; giving up and continuing shutdown");
        }
    });
}

/// Cap on how long the exit waits for the history worker to drain
const HISTORY_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Initializes tracing: writes to stderr, level controlled by RUST_LOG
/// (info by default)
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Waits for a termination signal: SIGTERM + Ctrl-C on unix, Ctrl-C elsewhere
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

/// Raises the main window (shared by the tray, single-instance and panel
/// footer entry points)
pub fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Same reason as the about window: the backdrop behind an unpainted
        // webview has to match the theme in force *now*, or reopening flashes
        // the previous one (see commands::theme_backdrop)
        let _ = window.set_background_color(Some(commands::theme_backdrop(app)));
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Minimum settings window height (logical pixels; same value as minHeight in
/// Tauri.toml)
const SETTINGS_MIN_HEIGHT: f64 = 320.0;
/// Maximum settings window height as a fraction of the monitor height (leaves
/// room for the menu bar / taskbar plus some breathing space)
const SETTINGS_MAX_HEIGHT_RATIO: f64 = 0.88;

/// Fits the settings window height to its content: a category page with
/// little in it shrinks the window instead of leaving a large blank area
///
/// Only the height changes, never the width (a width the user dragged is
/// preserved); the frontend measures the webview content height, so this adds
/// the window decoration (title bar) height and clamps to
/// [minimum, usable monitor height] — with very long content the window stays
/// on screen and the main area scrolls itself.
pub fn resize_settings_window_impl(app: &tauri::AppHandle, content_height: f64) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(inner), Ok(outer), Ok(scale)) = (
        window.inner_size(),
        window.outer_size(),
        window.scale_factor(),
    ) else {
        return;
    };
    // Decoration height = outer frame - content area (the title bar); the
    // frontend measures the content area height
    let decoration = outer.height.saturating_sub(inner.height);
    let mut target = (content_height * scale) as u32 + decoration;
    if let Ok(Some(monitor)) = window.current_monitor() {
        let max = (f64::from(monitor.size().height) * SETTINGS_MAX_HEIGHT_RATIO) as u32;
        target = target.min(max);
    }
    target = target.max((SETTINGS_MIN_HEIGHT * scale) as u32 + decoration);
    // set_size sets the client (inner) size, so compare and preserve inner
    // dimensions. Feeding outer.width back in grew the window by the frame
    // border width on every call on Windows (outer is wider than inner
    // there; on macOS the two are equal, which hid the bug) and the
    // ResizeObserver round-trip turned that into unbounded growth.
    // Jitter guard: a 1px difference does not trigger set_size (keeps it
    // from feeding back into the frontend ResizeObserver)
    if target.abs_diff(inner.height) < 2 {
        return;
    }
    let _ = window.set_size(tauri::PhysicalSize::new(inner.width, target));
}

/// Where the panel waits while hidden (physical pixels, far outside every
/// display)
///
/// **Positioning a window is queued; showing one is immediate.** tao moves a
/// window through `set_frame_top_left_point_async`, which hands the work to
/// the main dispatch queue, while `set_visible(true)` runs
/// `make_key_and_order_front_sync` on the spot. So however carefully
/// show_panel works out a position first, `show()` still paints one frame
/// wherever the window last sat, and only then does the queued move land.
///
/// That frame cannot be avoided from here, so it is aimed where nobody can
/// see it. It matters most on the very first open: "wherever it last sat" is
/// then the placement the system picked when the window was created —
/// measured at (-1470, 237) on a two-display setup, a whole screen away from
/// the tray, which is exactly the window that appeared to flash and vanish.
const PANEL_PARKING: (i32, i32) = (-30000, -30000);

/// Moves a floating window to the off-screen parking spot (see
/// `PANEL_PARKING`)
fn park_window(window: &tauri::WebviewWindow) {
    let _ = window.set_position(tauri::PhysicalPosition::new(
        PANEL_PARKING.0,
        PANEL_PARKING.1,
    ));
}

/// Parks the hidden panel off screen (see `PANEL_PARKING`); every path that
/// hides the panel has to call this, or the next open flashes a frame at the
/// spot it was last shown
fn park_panel(app: &tauri::AppHandle) {
    if let Some(panel) = app.get_webview_window("panel") {
        park_window(&panel);
    }
}

/// The tray icon's rectangle (physical pixels)
#[derive(Clone, Copy)]
struct IconRect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

/// Where the panel goes when the tray icon opens it (pure geometry, so the
/// cases that need a real tray to reproduce are unit tested)
///
/// The panel sits **beside the icon, growing away from the screen edge the
/// icon is on** — the shape every tray flyout has. Which edge that is comes
/// from the icon's position within the work area, not from the platform:
/// macOS puts its menu bar at the top, Windows puts the taskbar at the bottom
/// by default but the user can move it to any side.
///
/// Anchoring to the icon rather than to the pointer is what makes this
/// dependable. Pointer placement only reached the corner by way of the clamp,
/// so it needed the pointer to be near the edge — and it is not, whenever the
/// icon is opened from the Windows hidden-icon flyout, which sits well inside
/// the screen. That left the panel stranded mid-screen.
fn place_panel_at_tray(icon: IconRect, panel: (i32, i32), area: MonitorRect) -> (i32, i32) {
    let (panel_w, panel_h) = panel;
    let (icon_cx, icon_cy) = (
        (icon.left + icon.right) / 2.0,
        (icon.top + icon.bottom) / 2.0,
    );
    // Vertical: below the icon when it is in the upper half (a menu bar), above
    // it when in the lower half (a taskbar along the bottom)
    let below_icon = icon_cy < f64::from(area.top + area.bottom) / 2.0;
    let y = if below_icon {
        icon.bottom as i32
    } else {
        icon.top as i32 - panel_h
    };
    // Horizontal: line the panel's near edge up with the icon's, so it opens
    // inward from whichever side the icon sits on
    let x = if icon_cx < f64::from(area.left + area.right) / 2.0 {
        icon.left as i32
    } else {
        icon.right as i32 - panel_w
    };
    (
        x.clamp(area.left, (area.right - panel_w).max(area.left)),
        y.clamp(area.top, (area.bottom - panel_h).max(area.top)),
    )
}

/// What the panel is positioned against when it opens
#[derive(Clone, Copy)]
enum PanelAnchor {
    /// The tray icon's own rectangle, carried by the click event
    ///
    /// Physical pixels: `tray-icon` reports the rect as PhysicalPosition /
    /// PhysicalSize on every platform, so the conversion out of `tauri::Rect`
    /// is lossless whatever scale factor it is given.
    Tray(tauri::Rect),
    /// The pointer — the global hotkey has no icon to sit next to, and
    /// wherever the user is working is the least surprising place for it
    Cursor,
}

/// Opens the history panel: positions it, then shows and focuses it
///
/// Whatever the anchor, the result is clamped into the **work area** of the
/// monitor it lands on, never the full monitor: the tray sits inside the
/// Windows taskbar, and clamping to the monitor put the panel over the
/// taskbar and the very icon that opened it. The OS does not pull a window
/// back on screen when the application placed it with set_position, so
/// nothing else catches an overflow.
fn show_panel(app: &tauri::AppHandle, anchor: PanelAnchor) {
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };
    // Toggle semantics: triggering again while the panel is in the
    // foreground dismisses it
    if panel.is_visible().unwrap_or(false) && panel.is_focused().unwrap_or(false) {
        hide_panel_impl(app);
        return;
    }
    // The blur→click race of a tray click: it was just hidden on blur, so
    // this trigger counts as the closing half of a toggle
    let since_blur = lanecho_core::clipboard::now_ms()
        .saturating_sub(LAST_PANEL_BLUR_HIDE_MS.load(std::sync::atomic::Ordering::Relaxed));
    if since_blur < 300 {
        return;
    }
    // The fallback is a logical size (380×480 from Tauri.toml) and has to be
    // scaled into the physical coordinate space
    let panel_size = panel.outer_size().unwrap_or_else(|_| {
        let scale = panel.scale_factor().unwrap_or(1.0);
        tauri::PhysicalSize::new((380.0 * scale) as u32, (480.0 * scale) as u32)
    });
    // The point used to pick the monitor, and — for a tray anchor — the icon
    // to sit beside
    let icon = match anchor {
        PanelAnchor::Tray(rect) => {
            let pos = rect.position.to_physical::<f64>(1.0);
            let size = rect.size.to_physical::<u32>(1.0);
            Some(IconRect {
                left: pos.x,
                top: pos.y,
                right: pos.x + f64::from(size.width),
                bottom: pos.y + f64::from(size.height),
            })
        }
        PanelAnchor::Cursor => None,
    };
    let reference = match icon {
        Some(icon) => Some((
            (icon.left + icon.right) / 2.0,
            (icon.top + icon.bottom) / 2.0,
        )),
        None => app.cursor_position().ok().map(|pos| (pos.x, pos.y)),
    };
    if let Some((ref_x, ref_y)) = reference {
        let (mut x, mut y) = (ref_x, ref_y);
        if let Ok(Some(monitor)) = app.monitor_from_point(ref_x, ref_y) {
            let area = monitor.work_area();
            let area = MonitorRect {
                left: area.position.x,
                top: area.position.y,
                right: area.position.x + area.size.width as i32,
                bottom: area.position.y + area.size.height as i32,
            };
            let placed = match icon {
                Some(icon) => place_panel_at_tray(
                    icon,
                    (panel_size.width as i32, panel_size.height as i32),
                    area,
                ),
                // The pointer anchors the panel's top-left corner and it
                // expands down and right from there, the way it always has
                None => (
                    (ref_x as i32).clamp(
                        area.left,
                        (area.right - panel_size.width as i32).max(area.left),
                    ),
                    (ref_y as i32).clamp(
                        area.top,
                        (area.bottom - panel_size.height as i32).max(area.top),
                    ),
                ),
            };
            (x, y) = (f64::from(placed.0), f64::from(placed.1));
        }
        let _ = panel.set_position(tauri::PhysicalPosition::new(x, y));
    } else {
        // The panel waits off screen while hidden, so a position **must** be
        // set on every open — without this fallback a failed cursor read
        // would leave it parked where it can never be seen again
        let _ = panel.set_position(tauri::PhysicalPosition::new(40, 40));
    }
    let _ = panel.show();
    let _ = panel.set_focus();
}

/// Windows that block yielding when the panel is dismissed (macOS): if any of
/// them is visible, `app.hide()` is not called
///
/// `app.hide()` hides the **whole application**, not just the panel, so every
/// window visible at that moment goes with it. "Preferences" and "About" in
/// the panel footer both open a window and then dismiss the panel, so a
/// missing entry here makes the window vanish the instant it appears and only
/// come back on the next tray click. **A new long-lived window must be
/// registered here.**
#[cfg(target_os = "macos")]
const YIELD_BLOCKING_WINDOWS: [&str; 2] = ["main", "about"];

/// Dismisses the history panel (shared by Esc, picking an entry and toggle)
///
/// On macOS, with no other window open, the whole application yields too:
/// NSApp.hide hands focus back to the previous application, so "open → pick →
/// ⌘V" runs in one go without the user clicking back into the target
/// application. It does not yield while a window is open (the user may be
/// going back and forth between that window and the panel); see
/// `YIELD_BLOCKING_WINDOWS`.
pub fn hide_panel_impl(app: &tauri::AppHandle) {
    hide_preview_impl(app); // the card is dismissed along with the panel
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
    park_panel(app);
    #[cfg(target_os = "macos")]
    {
        let any_visible = YIELD_BLOCKING_WINDOWS.iter().any(|label| {
            app.get_webview_window(label)
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false)
        });
        if !any_visible {
            let _ = app.hide();
        }
    }
}

/// Logical width / default height of the preview card (kept in sync with the
/// preview window config in Tauri.toml); the real height arrives through
/// show_preview once the card has measured its content, so short content
/// gets a small card
const PREVIEW_SIZE: (f64, f64) = (400.0, 440.0);
/// Height bounds of the preview card (logical pixels): the cap matches the
/// panel height so the card never visually towers over it
const PREVIEW_HEIGHT_RANGE: (f64, f64) = (100.0, 480.0);

/// Shows the preview card: picks a side from the space actually available on
/// either side of the panel and keeps the whole card on screen
///
/// `height` is the content height the card measured itself (logical pixels,
/// None means the default); the size is set before positioning. The window is
/// a pure presentation layer with focusable=false and cursor pass-through, so
/// show never steals focus — the panel keeps keyboard focus, and neither
/// hide-on-blur nor the ⌘V focus handback is affected.
pub fn show_preview_impl(app: &tauri::AppHandle, anchor_y: Option<f64>, height: Option<f64>) {
    let (Some(panel), Some(preview)) = (
        app.get_webview_window("panel"),
        app.get_webview_window("preview"),
    ) else {
        return;
    };
    // A late hover callback: do not pop the card once the panel is gone
    // (no orphan floating window)
    if !panel.is_visible().unwrap_or(false) {
        return;
    }
    let (Ok(panel_pos), Ok(panel_size)) = (panel.outer_position(), panel.outer_size()) else {
        return;
    };
    let scale = panel.scale_factor().unwrap_or(1.0);
    // The height follows the content; positioning converts from the requested
    // logical size rather than reading outer_size (right after set_size that
    // may still read back the old value)
    let wanted_h = height.unwrap_or(PREVIEW_SIZE.1);
    let logical_h = wanted_h.clamp(PREVIEW_HEIGHT_RANGE.0, PREVIEW_HEIGHT_RANGE.1);
    let _ = preview.set_size(tauri::LogicalSize::new(PREVIEW_SIZE.0, logical_h));
    // Cursor pass-through is only taken back when the height cap truncates
    // the content, so the wheel can scroll the card to read the rest; content
    // that fits keeps pass-through (a pure presentation layer must not eat
    // clicks meant for the window below). Taking it back does not affect
    // focus: the window has focusable=false, so canBecomeKeyWindow is always
    // NO on macOS and even a click on the card cannot take keyboard focus
    // from the panel — hide-on-blur and the ⌘V handback still hold.
    //
    // Not on Windows: flipping this flag while the card is visible makes tao
    // re-run ShowWindow(SW_SHOW), which activates the card and blurs the
    // panel shut (see hide_preview_impl). Pass-through stays permanently on
    // there — a truncated card cannot be wheel-scrolled on Windows
    #[cfg(not(windows))]
    let _ = preview.set_ignore_cursor_events(wanted_h <= PREVIEW_HEIGHT_RANGE.1);
    let (card_w, card_h) = ((PREVIEW_SIZE.0 * scale) as i32, (logical_h * scale) as i32);
    let gap = (8.0 * scale) as i32;
    let panel_left = panel_pos.x;
    let panel_right = panel_pos.x + panel_size.width as i32;
    // Align with the highlighted row (anchor_y is a logical coordinate inside
    // the panel document and is converted to physical here)
    let mut y = panel_pos.y + anchor_y.map_or(0, |ay| (ay * scale) as i32);
    let mut x = panel_right + gap;

    // The monitor the panel is on: by centre point first; when that fails
    // (the point falls on no monitor, and so on) fall back to the panel's
    // current monitor — skipping the clamps entirely would fling the whole
    // card off screen
    let center = (
        f64::from(panel_left) + f64::from(panel_size.width) / 2.0,
        f64::from(panel_pos.y) + f64::from(panel_size.height) / 2.0,
    );
    let monitor = app
        .monitor_from_point(center.0, center.1)
        .ok()
        .flatten()
        .or_else(|| panel.current_monitor().ok().flatten());

    if let Some(monitor) = monitor {
        let placed = place_preview(
            PreviewLayout {
                panel_left,
                panel_right,
                card_w,
                card_h,
                gap,
                desired_y: y,
            },
            // Work area, not the full monitor: the card must stay off the
            // Windows taskbar (and the macOS menu bar / Dock) just like the
            // panel does
            {
                let area = monitor.work_area();
                MonitorRect {
                    left: area.position.x,
                    top: area.position.y,
                    right: area.position.x + area.size.width as i32,
                    bottom: area.position.y + area.size.height as i32,
                }
            },
        );
        (x, y) = placed;
    }
    let _ = preview.set_position(tauri::PhysicalPosition::new(x, y));
    if !preview.is_visible().unwrap_or(false) {
        let _ = preview.show();
    }
}

/// Inputs for positioning the preview card (all physical pixels)
struct PreviewLayout {
    /// Left / right edge of the panel
    panel_left: i32,
    panel_right: i32,
    /// Card width / height
    card_w: i32,
    card_h: i32,
    /// Gap between the card and the panel
    gap: i32,
    /// Desired y, aligned with the highlighted row
    desired_y: i32,
}

/// Visible rectangle of the monitor (physical pixels)
#[derive(Clone, Copy)]
struct MonitorRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Computes where the preview card lands (pure geometry, so unit tests can
/// cover the edge cases)
///
/// Side selection: prefer the right of the panel; fall back to the left when
/// the card does not fit on the right; when neither side fits take the wider
/// one and let the two-way clamp push it back on screen — overlapping the
/// panel beats running off the screen.
fn place_preview(layout: PreviewLayout, mon: MonitorRect) -> (i32, i32) {
    let space_right = mon.right - (layout.panel_right + layout.gap);
    let space_left = (layout.panel_left - layout.gap) - mon.left;
    let put_right =
        space_right >= layout.card_w || (space_left < layout.card_w && space_right >= space_left);
    let x = if put_right {
        layout.panel_right + layout.gap
    } else {
        layout.panel_left - layout.gap - layout.card_w
    };
    (
        x.min(mon.right - layout.card_w).max(mon.left),
        layout
            .desired_y
            .min(mon.bottom - layout.card_h)
            .max(mon.top),
    )
}

/// Hides the preview card (follows the panel being dismissed, losing focus,
/// or the highlighted row going away)
///
/// On Windows it parks off screen instead of hiding: whenever any window flag
/// changes while the visible flag is set, tao re-runs `ShowWindow(SW_SHOW)`,
/// which activates the window even with `WS_EX_NOACTIVATE` — re-showing the
/// card would steal focus from the panel and trigger its hide-on-blur. By
/// never clearing the visible flag after the first show (which `focus =
/// false` in Tauri.toml turns into a non-activating `SW_SHOWNOACTIVATE`), no
/// `ShowWindow` call ever happens again.
pub fn hide_preview_impl(app: &tauri::AppHandle) {
    if let Some(preview) = app.get_webview_window("preview")
        && preview.is_visible().unwrap_or(false)
    {
        #[cfg(windows)]
        park_window(&preview);
        #[cfg(not(windows))]
        let _ = preview.hide();
    }
}

/// Registers the global hotkeys from the settings (unregister everything
/// first; shared by startup and settings changes)
///
/// Returns Err when the panel hotkey fails to parse or register (the settings
/// page reports it as hotkey_invalid); a slot hotkey (slotModifier+1..6)
/// taken by another application is warned about and skipped one by one
/// instead of failing the whole call.
pub fn apply_hotkeys(app: &tauri::AppHandle, settings: &settings::Settings) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    let mut parsed = state::ParsedHotkeys::default();
    if !settings.panel_hotkey.is_empty() {
        let shortcut: Shortcut = settings
            .panel_hotkey
            .parse()
            .map_err(|e| format!("{e:?}"))?;
        shortcuts.register(shortcut).map_err(|e| e.to_string())?;
        parsed.panel = Some(shortcut);
    }
    // Register slot by slot and record the failures (taken by another
    // application): the settings page uses this to tell the user, otherwise
    // the toggle reads as on while the keys do nothing and the feature just
    // looks broken
    let mut failures = Vec::new();
    if settings.slot_hotkeys {
        let modifier = &settings.slot_modifier;
        for n in 1..=6u8 {
            let slot: Result<Shortcut, _> = format!("{modifier}+{n}").parse();
            match slot {
                Ok(shortcut) => {
                    if let Err(e) = shortcuts.register(shortcut) {
                        tracing::warn!(
                            "Failed to register slot hotkey {modifier}+{n} (possibly in use): {e}"
                        );
                        failures.push(n);
                    } else {
                        parsed.slots.push((shortcut, n));
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to parse slot hotkey {modifier}+{n}: {e:?}");
                    failures.push(n);
                }
            }
        }
    }
    let state = app.state::<AppState>();
    *lock(&state.slot_hotkey_failures) = failures;
    // The parse cache lands together with the registration state: the
    // dispatch handler no longer parses on every keypress
    *lock(&state.hotkeys) = parsed;
    Ok(())
}

/// Global hotkey dispatch: open the panel / paste a slot directly (matched
/// against the parse results cached at registration)
fn handle_shortcut(app: &tauri::AppHandle, shortcut: &tauri_plugin_global_shortcut::Shortcut) {
    let state = app.state::<AppState>();
    let slot_n = {
        let hotkeys = lock(&state.hotkeys);
        if hotkeys.panel.as_ref() == Some(shortcut) {
            drop(hotkeys);
            show_panel(app, PanelAnchor::Cursor);
            return;
        }
        hotkeys
            .slots
            .iter()
            .find(|(slot, _)| slot == shortcut)
            .map(|(_, n)| *n)
    };
    let Some(n) = slot_n else { return };
    // Slot N = the Nth entry under the panel's current sort order; only the
    // ID is taken under the lock, instead of cloning the whole history table
    // along with its full text
    let sort = lock(&state.settings).history_sort.clone();
    let Some(id) = state.history.entry_id_at(&sort, usize::from(n) - 1) else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = commands::copy_entry_to_clipboard(&app, &id).await {
            tracing::warn!("Failed to paste from slot: {e}");
        }
    });
}

/// Handles to the tray menu items (for in-place updates, managed after
/// setup_tray; Linux only — on other platforms the menu moved to the bottom
/// of the panel)
///
/// On Linux `set_menu` cannot replace the menu wholesale (the tauri docs are
/// explicit: once set, a menu can be neither removed nor replaced, only its
/// content changed), so the checked state and the localized text are all
/// updated in place through these handles.
#[cfg(target_os = "linux")]
struct TrayMenu {
    /// Sync toggle (checkable)
    sync: CheckMenuItem<tauri::Wry>,
    /// History panel entry (the Linux tray fires no left-click event, so the
    /// menu is the main entry point there and this comes first)
    history: MenuItem<tauri::Wry>,
    /// Open settings
    settings: MenuItem<tauri::Wry>,
    /// About (opens the "About" category of the settings window)
    about: MenuItem<tauri::Wry>,
    /// Quit
    quit: MenuItem<tauri::Wry>,
}

/// Refreshes the tray menu in place (call it after a language, sync toggle or
/// incognito change; the native menu only exists on Linux and this is a no-op
/// elsewhere, so callers need not care about the platform)
pub fn refresh_tray_menu(app: &tauri::AppHandle) {
    #[cfg(target_os = "linux")]
    {
        let Some(items) = app.try_state::<TrayMenu>() else {
            return;
        };
        let texts = locale::current(app);
        let state = app.state::<AppState>();
        let _ = items.sync.set_text(texts.tray_sync);
        let _ = items.sync.set_checked(lock(&state.settings).sync_enabled);
        let _ = items.history.set_text(texts.tray_history);
        let _ = items.settings.set_text(texts.tray_settings);
        let _ = items.about.set_text(texts.tray_about);
        let _ = items.quit.set_text(texts.tray_quit);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
    }
}

/// Settings string → engine sync direction policy (settings.normalize already
/// guarantees a valid value; the fallback here is defensive — a parse failure
/// means both, so a dirty value cannot switch sync off entirely)
pub(crate) fn sync_mode_of(settings: &settings::Settings) -> lanecho_core::sync::SyncMode {
    lanecho_core::sync::SyncMode::parse(&settings.sync_mode)
        .unwrap_or(lanecho_core::sync::SyncMode::Both)
}

/// Settings → engine type toggles
pub(crate) fn sync_types_of(settings: &settings::Settings) -> lanecho_core::sync::SyncTypes {
    lanecho_core::sync::SyncTypes {
        text: settings.sync_text,
        images: settings.sync_images,
        files: settings.sync_files,
    }
}

/// Settings → file sync cap in bytes (normalize already clamps it to
/// 1~512MB)
pub(crate) fn max_sync_file_bytes_of(settings: &settings::Settings) -> u64 {
    u64::from(settings.max_sync_file_mb) * 1024 * 1024
}

/// Refreshes the tray tooltip with the number of pending pairing requests
///
/// The Accessory form has no Dock badge to use and system notifications are
/// transient — the tray tooltip is the only persistent hint left once a
/// notification is missed.
pub fn update_pending_tooltip(app: &tauri::AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let pending = app.state::<AppState>().engine.pending_pair_requests().len();
    let tooltip = if pending > 0 {
        locale::current(app).tray_pending(pending)
    } else {
        "Lanecho".to_string()
    };
    let _ = tray.set_tooltip(Some(tooltip));
}

/// Toggles sync from the tray menu (shares settings.syncMode with the
/// settings window; Linux only)
///
/// Same shape as save_settings: persist first, apply side effects only after
/// the write succeeds; on failure nothing takes effect at all (the tray has
/// no channel for showing errors, and keeping the old state beats a
/// half-applied one).
///
/// Degraded semantics under the direction policy: off ↔ both — clicking while
/// in send or receive mode switches to off, and clicking back gives both, so
/// **the direction is lost** and a Linux user has to restore it from the
/// settings page. The tray checkbox has only two states; this is a deliberate
/// trade-off.
#[cfg(target_os = "linux")]
fn toggle_sync_from_tray(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let mut next = lock(&state.settings).clone();
    next.sync_mode = if next.sync_mode == "off" {
        "both"
    } else {
        "off"
    }
    .to_string();
    next.normalize();
    if let Err(e) = next.save(&state.data_dir) {
        tracing::warn!("Failed to persist sync toggle; this change will not take effect: {e}");
        refresh_tray_menu(app); // put the checkmark back to the real state
        return;
    }
    let mode = sync_mode_of(&next);
    let mode_str = next.sync_mode.clone();
    // off→both means the sync pipeline is back: reset the watcher's dedup
    // baseline (same cause and same treatment as sync_resumed in
    // save_settings — content copied while it was off must be sendable again
    // once it is back on)
    if mode_str != "off" {
        state
            .reset_dedupe
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    *lock(&state.settings) = next;
    state.engine.set_sync_mode(mode);
    refresh_tray_menu(app);
    // Payload = the syncMode string (the frontend maps a bool payload back
    // coarsely to both, see save_settings)
    let _ = app.emit(bridge::events::SYNC_STATE, mode_str);
}

/// Creates the system tray: both mouse buttons open the history panel — what
/// used to be the tray context menu now lives at the bottom of the panel.
/// Linux is the exception: its tray fires no click event (the tauri docs are
/// explicit), so the native menu is still the only entry point there and is
/// kept in full.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Lanecho")
        .on_tray_icon_event(|tray, event| {
            // Both buttons open the panel (the panel is the main interaction
            // surface and its menu sits at the bottom); Linux never fires
            // this event and goes through the native menu kept below
            if let TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                // The icon's own rectangle, not the pointer: the pointer is
                // somewhere inside the hidden-icon flyout when the tray icon
                // is opened from there, which is nowhere near the screen edge
                show_panel(tray.app_handle(), PanelAnchor::Tray(rect));
            }
        });

    #[cfg(target_os = "linux")]
    {
        let texts = locale::current(app);
        let state = app.state::<AppState>();
        let items = TrayMenu {
            sync: CheckMenuItem::with_id(
                app,
                "toggle_sync",
                texts.tray_sync,
                true,
                lock(&state.settings).sync_enabled,
                None::<&str>,
            )?,
            history: MenuItem::with_id(app, "history", texts.tray_history, true, None::<&str>)?,
            settings: MenuItem::with_id(app, "settings", texts.tray_settings, true, None::<&str>)?,
            about: MenuItem::with_id(app, "about", texts.tray_about, true, None::<&str>)?,
            quit: MenuItem::with_id(app, "quit", texts.tray_quit, true, None::<&str>)?,
        };
        // "History" comes first: the top item is how a Linux user opens the
        // panel
        let menu = Menu::with_items(
            app,
            &[
                &items.history,
                &items.sync,
                &items.settings,
                &items.about,
                &items.quit,
            ],
        )?;
        app.manage(items);
        tray = tray
            .menu(&menu)
            // Left click raises a window, right click pops the menu (without
            // this a left click pops the menu; on Linux only the menu exists)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id().as_ref() {
                "toggle_sync" => toggle_sync_from_tray(app),
                // The Linux tray fires no click event, so this menu item is
                // the only way in and carries no icon rectangle
                "history" => show_panel(app, PanelAnchor::Cursor),
                "settings" => show_main_window(app),
                // show_about is an async command (lazy window creation has to
                // avoid the Windows deadlock of building a webview
                // synchronously on the main thread), so the menu callback
                // spawns it onto the runtime
                "about" => {
                    tauri::async_runtime::spawn(commands::show_about(app.clone()));
                }
                "quit" => app.exit(0),
                _ => {}
            });
    }

    // macOS uses a monochrome template icon (tinted automatically with the
    // system appearance); fall back to the application icon if it fails to
    // decode
    #[cfg(target_os = "macos")]
    {
        match tauri::image::Image::from_bytes(include_bytes!("../icons/tray-iconTemplate.png")) {
            Ok(template) => {
                tray = tray.icon(template).icon_as_template(true);
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to decode tray template icon; falling back to application icon: {e}"
                );
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

#[cfg(test)]
mod tray_panel_layout_tests {
    use super::*;

    /// Panel size used throughout (380×480, as configured)
    const PANEL: (i32, i32) = (380, 480);

    /// A 1920×1080 screen whose work area stops 40px short of the bottom —
    /// a Windows taskbar along the bottom edge
    fn taskbar_bottom() -> MonitorRect {
        MonitorRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }
    }

    /// A tray icon rectangle, 24×24, at the given top-left
    fn icon(x: f64, y: f64) -> IconRect {
        IconRect {
            left: x,
            top: y,
            right: x + 24.0,
            bottom: y + 24.0,
        }
    }

    /// Windows, tray at the bottom right: the panel sits above the icon with
    /// their right edges lined up — never over the taskbar, never mid-screen
    ///
    /// The icon itself lives *inside* the taskbar, below the work area, so the
    /// clamp is what settles the panel's bottom edge: flush against the top of
    /// the taskbar, which is the closest it can get to the icon
    #[test]
    fn opens_above_a_bottom_right_tray_icon() {
        let area = taskbar_bottom();
        let (x, y) = place_panel_at_tray(icon(1850.0, 1048.0), PANEL, area);
        assert_eq!(x + PANEL.0, 1874, "right edges line up with the icon");
        assert_eq!(y + PANEL.1, area.bottom, "bottom sits on the taskbar");
        assert!(y >= area.top, "and the top is still on screen");
    }

    /// An icon sitting a few slots in from the edge — the common case, with
    /// other tray icons and the clock to its right — opens against the icon
    /// rather than against the screen edge
    #[test]
    fn follows_an_icon_that_is_not_in_the_very_corner() {
        let area = taskbar_bottom();
        let (x, y) = place_panel_at_tray(icon(1700.0, 1048.0), PANEL, area);
        assert_eq!(
            x + PANEL.0,
            1724,
            "right edge tracks the icon, not the screen"
        );
        assert_eq!(y + PANEL.1, area.bottom);
        assert!(
            x + PANEL.0 < area.right,
            "leaves the icons to its right uncovered"
        );
    }

    /// macOS, menu bar icon at the top right: the panel drops below the icon,
    /// right edges lined up — the placement the native client also produces
    #[test]
    fn drops_below_a_top_right_menu_bar_icon() {
        let area = MonitorRect {
            left: 0,
            top: 25,
            right: 1512,
            bottom: 982,
        };
        let (x, y) = place_panel_at_tray(icon(1450.0, 0.0), PANEL, area);
        assert_eq!(y, 25, "below the menu bar, clamped to the work area top");
        assert_eq!(x, 1474 - 380);
    }

    /// A taskbar moved to the left edge puts the tray at the bottom left: the
    /// panel opens upward and to the right, hugging that corner instead
    #[test]
    fn opens_from_a_bottom_left_tray() {
        let area = MonitorRect {
            left: 60,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let (x, y) = place_panel_at_tray(icon(70.0, 1050.0), PANEL, area);
        assert_eq!(x, 70, "left edges line up");
        // No bottom inset here — the taskbar is down the left side — so the
        // panel rests directly on the icon instead of on a work-area edge
        assert_eq!(y + PANEL.1, 1050, "bottom sits on the icon");
    }

    /// A screen shorter than the panel cannot fit it: the clamp still yields a
    /// point inside the work area rather than an inverted range
    #[test]
    fn survives_a_work_area_smaller_than_the_panel() {
        let tiny = MonitorRect {
            left: 0,
            top: 0,
            right: 300,
            bottom: 300,
        };
        let (x, y) = place_panel_at_tray(icon(280.0, 280.0), PANEL, tiny);
        assert_eq!((x, y), (0, 0));
    }
}

#[cfg(test)]
mod preview_layout_tests {
    use super::*;

    /// A 1440×900 logical screen (origin 0,0)
    fn screen() -> MonitorRect {
        MonitorRect {
            left: 0,
            top: 0,
            right: 1440,
            bottom: 900,
        }
    }

    /// Panel spans [left, left+380], card is 400×300
    fn layout(panel_left: i32, desired_y: i32) -> PreviewLayout {
        PreviewLayout {
            panel_left,
            panel_right: panel_left + 380,
            card_w: 400,
            card_h: 300,
            gap: 8,
            desired_y,
        }
    }

    /// Panel on the left: plenty of room on the right, the card hugs its
    /// right edge
    #[test]
    fn prefers_right_side_when_it_fits() {
        let (x, _) = place_preview(layout(0, 100), screen());
        assert_eq!(x, 388);
    }

    /// Panel on the right: 400 wide does not fit there, so it flips to the
    /// left of the panel and stays fully on screen
    #[test]
    fn flips_left_when_right_is_too_narrow() {
        // Panel right edge at 1240, only 192 < 400 left on the right
        let (x, _) = place_preview(layout(860, 100), screen());
        assert_eq!(
            x, 452,
            "Should land to the left of the panel: 860 - 8 - 400"
        );
        assert!(x >= 0 && x + 400 <= 1440);
    }

    /// Very narrow screen: with neither side fitting the card must still be
    /// fully on screen (regression: clamping only the left edge is not enough)
    #[test]
    fn never_overflows_on_narrow_screen() {
        let narrow = MonitorRect {
            left: 0,
            top: 0,
            right: 600,
            bottom: 900,
        };
        let (x, _) = place_preview(layout(100, 100), narrow);
        assert!(x >= 0, "Must not cross the left boundary");
        assert!(x + 400 <= 600, "Must not cross the right boundary");
    }

    /// Secondary monitor (negative origin): the bounds come from that
    /// monitor, not from the primary one
    #[test]
    fn respects_secondary_monitor_origin() {
        let left_monitor = MonitorRect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let (x, _) = place_preview(layout(-400, 100), left_monitor);
        assert!(x >= -1920 && x + 400 <= 0);
    }

    /// Highlighted row near the bottom of the screen: the card moves up until
    /// it is fully visible
    #[test]
    fn clamps_bottom_edge() {
        let (_, y) = place_preview(layout(0, 880), screen());
        assert_eq!(y, 600, "900 - 300");
    }
}

#[cfg(test)]
mod csp_tests {
    /// The CSP must explicitly allow the IPC channel, or binary IPC
    /// (`ipc::Response`) in a packaged build silently falls back to the
    /// postMessage channel, which on macOS turns byte arrays into JSON number
    /// arrays and renders preview card images as broken images — dev has no
    /// CSP at all so everything looks fine there and it only shows up once
    /// installed, hence this test (full rationale in the comment on that key
    /// in Tauri.toml)
    #[test]
    fn csp_allows_the_ipc_channel() {
        let config = include_str!("../Tauri.toml");
        let csp = config
            .lines()
            .find(|l| l.starts_with("csp = "))
            .expect("Tauri.toml is missing the csp setting");
        assert!(
            csp.contains("connect-src"),
            "CSP is missing connect-src: {csp}"
        );
        // macOS/Linux use ipc://localhost, Windows uses http://ipc.localhost
        assert!(
            csp.contains(" ipc:"),
            "CSP does not allow the ipc: scheme: {csp}"
        );
        assert!(
            csp.contains("http://ipc.localhost"),
            "CSP does not allow the Windows IPC origin: {csp}"
        );
    }
}
