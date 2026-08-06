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

mod bridge;
mod commands;
mod history;
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
            commands::window_effects_active,
            commands::get_slot_hotkey_failures,
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
    // Jitter guard: a 1px difference does not trigger set_size (keeps it from
    // feeding back into the frontend ResizeObserver)
    if target.abs_diff(outer.height) < 2 {
        return;
    }
    let _ = window.set_size(tauri::PhysicalSize::new(outer.width, target));
}

/// Opens the history panel: positions it near the cursor, then shows and
/// focuses it
///
/// It must be clamped to the edges of the monitor under the cursor: the tray
/// icon sits right in a screen corner (top right on macOS, bottom right on
/// Windows), so putting the panel's top-left corner at the cursor pushes the
/// whole panel off screen — the OS does not pull a window back on screen when
/// the application positioned it itself with set_position.
fn show_panel(app: &tauri::AppHandle) {
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
    if let Ok(pos) = app.cursor_position() {
        let (mut x, mut y) = (pos.x, pos.y);
        // The fallback is a logical size (380×480 from Tauri.toml) and has to
        // be scaled into the physical coordinate space
        let panel_size = panel.outer_size().unwrap_or_else(|_| {
            let scale = panel.scale_factor().unwrap_or(1.0);
            tauri::PhysicalSize::new((380.0 * scale) as u32, (480.0 * scale) as u32)
        });
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
    // from the panel — hide-on-blur and the ⌘V handback still hold
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
            MonitorRect {
                left: monitor.position().x,
                top: monitor.position().y,
                right: monitor.position().x + monitor.size().width as i32,
                bottom: monitor.position().y + monitor.size().height as i32,
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
pub fn hide_preview_impl(app: &tauri::AppHandle) {
    if let Some(preview) = app.get_webview_window("preview")
        && preview.is_visible().unwrap_or(false)
    {
        let _ = preview.hide();
    }
}

/// Registers the global hotkeys from the settings (unregister everything
/// first; shared by startup and settings changes)
///
/// Returns Err when the panel hotkey fails to parse or register (the settings
/// page reports it as hotkey_invalid); a slot hotkey (Alt+1..6) taken by
/// another application is warned about and skipped one by one instead of
/// failing the whole call.
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
        for n in 1..=6u8 {
            let slot: Result<Shortcut, _> = format!("Alt+{n}").parse();
            match slot {
                Ok(shortcut) => {
                    if let Err(e) = shortcuts.register(shortcut) {
                        tracing::warn!(
                            "Failed to register slot hotkey Alt+{n} (possibly in use): {e}"
                        );
                        failures.push(n);
                    } else {
                        parsed.slots.push((shortcut, n));
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to parse slot hotkey Alt+{n}: {e:?}");
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
            show_panel(app);
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
        "lanecho".to_string()
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
        .tooltip("lanecho")
        .on_tray_icon_event(|tray, event| {
            // Both buttons open the panel (the panel is the main interaction
            // surface and its menu sits at the bottom); Linux never fires
            // this event and goes through the native menu kept below
            if let TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_panel(tray.app_handle());
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
                "history" => show_panel(app),
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
