//! Tauri command layer: the entry point for `invoke` calls from the frontend.
//!
//! Every error is an [`ErrDto`] { code, detail }: the frontend renders the code
//! through its i18n errors section and appends detail untranslated.

use serde::Serialize;
use tauri::{Manager, State};

use lanecho_core::sync::SyncError;

use crate::bridge::events;
use crate::settings::Settings;
use crate::state::{AppState, lock};

/// Structured error DTO
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrDto {
    /// Stable error code (the key into the frontend's i18n errors section)
    pub code: &'static str,
    /// Raw detail (untranslated, appended verbatim by the presentation layer)
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
    /// Code only
    pub fn new(code: &'static str) -> Self {
        Self { code, detail: None }
    }

    /// Code plus detail
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
            // Structured rejection codes from the peer pass straight through
            // (not_paired / too_large / disabled / unsupported_type)
            SyncError::Rejected(code) => match code.as_str() {
                "not_paired" => ErrDto::new("not_paired"),
                "too_large" => ErrDto::new("too_large"),
                "disabled" => ErrDto::new("disabled"),
                "unsupported_type" => ErrDto::new("unsupported_type"),
                "checksum_mismatch" => ErrDto::new("checksum_mismatch"),
                "identity_mismatch" => ErrDto::new("identity_mismatch"),
                "unsupported_version" => ErrDto::new("unsupported_version"),
                other => ErrDto::with("rejected", other),
            },
            other => ErrDto::with("engine", other),
        }
    }
}

/// Local device info DTO
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfInfoDto {
    /// Display name
    pub name: String,
    /// Device ID
    pub device_id: String,
    /// Certificate fingerprint
    pub fingerprint: String,
    /// Platform identifier
    pub platform: String,
    /// Port actually being listened on
    pub port: u16,
}

/// Device list entry: merged view of online peers and paired devices, which
/// may be offline
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    /// Display name: the broadcast name while online, the name snapshotted at
    /// pairing time while offline
    pub name: String,
    /// Certificate fingerprint (the unique key)
    pub fingerprint: String,
    /// Platform identifier (known only while online)
    pub platform: Option<String>,
    /// OS version description (known only while online)
    pub os_version: Option<String>,
    /// Whether the device is currently online
    pub online: bool,
    /// Whether the device is paired
    pub paired: bool,
}

/// Read the settings
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    lock(&state.settings).clone()
}

/// Save the settings: **persist first, apply side effects only after the write
/// succeeds**. If the write fails, the engine, the in-memory copy and the tray
/// all keep their old state, so no half-applied split can appear.
#[tauri::command]
pub fn save_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<(), ErrDto> {
    let mut settings = settings;
    // Normalize (unknown mode falls back, syncEnabled is double-written, caps
    // are clamped): values arriving from the frontend are not trusted
    settings.normalize();
    let old = lock(&state.settings).clone();
    let language_changed = old.language != settings.language;
    let sync_changed = old.sync_mode != settings.sync_mode;
    let hotkeys_changed = old.panel_hotkey != settings.panel_hotkey
        || old.slot_hotkeys != settings.slot_hotkeys
        || old.slot_modifier != settings.slot_modifier;

    // Fallible steps come first: a failure fails the whole call, restores the
    // old state, and leaves no side effect behind
    if hotkeys_changed && let Err(e) = crate::apply_hotkeys(&app, &settings) {
        let _ = crate::apply_hotkeys(&app, &old);
        return Err(ErrDto::with("hotkey_invalid", e));
    }
    if let Err(e) = settings.save(&state.data_dir) {
        if hotkeys_changed {
            let _ = crate::apply_hotkeys(&app, &old);
        }
        return Err(ErrDto::with("settings_save_failed", e));
    }
    *lock(&state.settings) = settings.clone();

    // Everything below is an idempotent side effect, applied from the new
    // values
    if old.autostart != settings.autostart {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        let result = if settings.autostart {
            launcher.enable()
        } else {
            launcher.disable()
        };
        if let Err(e) = result {
            tracing::warn!("Failed to update autostart setting: {e}");
        }
    }
    if sync_changed {
        state.engine.set_sync_mode(crate::sync_mode_of(&settings));
        use tauri::Emitter;
        // The payload must be the syncMode string: a bool payload makes the
        // frontend map send/receive coarsely back to both, which rewrites the
        // radio selection when switching from off to send-only or
        // receive-only
        let _ = app.emit(events::SYNC_STATE, settings.sync_mode.clone());
    }
    // A record or sync pipeline was re-enabled (any gate going off to on):
    // reset the watcher's dedup baseline, so content copied while the gate was
    // closed is recorded and broadcast again on the next copy
    // (see settings::record_resumed / sync_resumed)
    if crate::settings::record_resumed(&old, &settings)
        || crate::settings::sync_resumed(&old, &settings)
    {
        state
            .reset_dedupe
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // Type toggles and the file cap take effect immediately (engine atomics,
    // no side-effect chain)
    state.engine.set_sync_types(crate::sync_types_of(&settings));
    state
        .engine
        .set_max_sync_file_bytes(crate::max_sync_file_bytes_of(&settings));
    // The watcher skips the image read only when both image toggles are off:
    // with "record images" off but "sync images" on the pixels are still
    // needed
    state.record_images.store(
        settings.history_record_images || settings.sync_images,
        std::sync::atomic::Ordering::Relaxed,
    );
    // Tray menu labels and check marks are fixed at creation time, so a
    // language or toggle change rebuilds the whole menu
    if language_changed || sync_changed {
        crate::refresh_tray_menu(&app);
    }
    Ok(())
}

/// Hot-update the display name (None or an empty string reverts to following
/// the hostname); identity.json is the single source of truth
#[tauri::command]
pub fn set_display_name(state: State<'_, AppState>, name: Option<String>) -> Result<(), ErrDto> {
    let name = name.filter(|n| !n.trim().is_empty());
    state
        .engine
        .set_display_name(name.as_deref())
        .map_err(|e| ErrDto::with("rename_failed", e))
}

/// Local device identity
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

/// Device list: online peers ∪ paired devices, the offline ones greyed out
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
    // Stable ordering: online first, then by name within each group
    devices.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));
    devices
}

/// Start pairing with a device; async because it waits for the peer's user to
/// confirm
#[tauri::command]
pub async fn pair_device(app: tauri::AppHandle, fingerprint: String) -> Result<(), ErrDto> {
    let state = app.state::<AppState>();
    state
        .engine
        .pair(&fingerprint)
        .await
        .map_err(|e| ErrDto::from(&e))
}

/// Respond to an inbound pairing request (paired with the pair-requested
/// event)
#[tauri::command]
pub fn respond_pair(app: tauri::AppHandle, fingerprint: String, accept: bool) {
    app.state::<AppState>()
        .engine
        .respond_pair(&fingerprint, accept);
    crate::update_pending_tooltip(&app);
}

/// Snapshot of the pending inbound pairing requests
///
/// Startup fallback: the event pump is ready before the frontend, so a
/// pair-requested event arriving in that window has no listener and is dropped,
/// leaving the peer to wait out the full timeout. The frontend pulls this on
/// mount. Every other event type already has a snapshot fallback such as
/// list_devices; only pairing requests lacked one.
#[tauri::command]
pub fn list_pending_pairs(state: State<'_, AppState>) -> Vec<crate::bridge::PeerDto> {
    state
        .engine
        .pending_pair_requests()
        .iter()
        .map(crate::bridge::PeerDto::from)
        .collect()
}

/// Unpair; takes effect locally at once and notifies the peer best-effort
#[tauri::command]
pub async fn unpair_device(app: tauri::AppHandle, fingerprint: String) -> Result<(), ErrDto> {
    let state = app.state::<AppState>();
    state.engine.unpair(&fingerprint).await;
    Ok(())
}

// ---- Clipboard history ----

/// Preview card text render cap, in characters: the truncation happens in Rust
/// and the frontend does not slice. Pushing a 5MB body into the DOM stalls
/// rendering, and there is no reason to ship it all just to display it.
const PREVIEW_TEXT_MAX_CHARS: usize = 20_000;

/// Preview card text payload (returned by `history_entry_text`)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryTextDto {
    /// Text, at most [`PREVIEW_TEXT_MAX_CHARS`] characters
    pub text: String,
    /// Total character count of the full text
    pub total_chars: usize,
    /// Whether the text was truncated
    pub truncated: bool,
}

/// History entry list; the sort order is read from the settings here, and
/// pinned entries always come first
///
/// Returns a **lightweight projection** with no inlined full text: the list is
/// refetched in full on every panel open and every copy, so carrying the bodies
/// (up to 5MB each) over IPC is the largest steady-state waste. Full text is
/// fetched on demand through [`history_entry_text`], search goes through
/// [`search_history`]. Sorting and serialization move off the main event loop
/// thread as well.
#[tauri::command]
pub async fn list_history(
    app: tauri::AppHandle,
) -> Result<Vec<crate::history::HistoryEntryMeta>, ErrDto> {
    let state = app.state::<AppState>();
    let sort = lock(&state.settings).history_sort.clone();
    let history = std::sync::Arc::clone(&state.history);
    tauri::async_runtime::spawn_blocking(move || history.list_meta(&sort))
        .await
        .map_err(|e| ErrDto::with("engine", e))
}

/// Full-text history search for the panel's search box: returns the set of
/// matching entry IDs, which the frontend filters in list order
///
/// Match semantics: lowercase containment over the preview plus the full text.
/// The bodies stay on the Rust side; searching no longer ships every entry's
/// full text to the frontend.
#[tauri::command]
pub async fn search_history(app: tauri::AppHandle, query: String) -> Result<Vec<String>, ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    tauri::async_runtime::spawn_blocking(move || history.search(&query))
        .await
        .map_err(|e| ErrDto::with("engine", e))
}

/// Fetch a text entry's preview card body by ID, truncated to the render cap
#[tauri::command]
pub async fn history_entry_text(app: tauri::AppHandle, id: String) -> Result<EntryTextDto, ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    let sliced = tauri::async_runtime::spawn_blocking(move || {
        history.entry_text(&id, PREVIEW_TEXT_MAX_CHARS)
    })
    .await
    .map_err(|e| ErrDto::with("engine", e))?;
    let (text, total_chars) = sliced.ok_or_else(|| ErrDto::new("history_missing"))?;
    Ok(EntryTextDto {
        truncated: total_chars > PREVIEW_TEXT_MAX_CHARS,
        text,
        total_chars,
    })
}

/// Hide the history panel; the frontend calls this on Esc or after an entry is
/// chosen
///
/// Funnelled through Rust because macOS also has to hand focus back to the
/// previous application to close the paste loop; see
/// [`crate::hide_panel_impl`].
#[tauri::command]
pub fn hide_panel(app: tauri::AppHandle) {
    crate::hide_panel_impl(&app);
}

/// Restore a chosen history entry into the system clipboard
///
/// Treated as a user copy: no echo registration, so once the watcher sees the
/// change it runs the normal local-copy pipeline (text broadcast plus the
/// history counter).
#[tauri::command]
pub async fn copy_history_entry(app: tauri::AppHandle, id: String) -> Result<(), ErrDto> {
    copy_entry_to_clipboard(&app, &id).await
}

/// Restore a history entry to the clipboard; shared by the command and the
/// slot hotkeys
pub async fn copy_entry_to_clipboard(app: &tauri::AppHandle, id: &str) -> Result<(), ErrDto> {
    use lanecho_core::clipboard;

    let (entry, history, restore_hash) = {
        let state = app.state::<AppState>();
        (
            state
                .history
                .entry(id)
                .ok_or_else(|| ErrDto::new("history_missing"))?,
            std::sync::Arc::clone(&state.history),
            std::sync::Arc::clone(&state.restore_hash),
        )
    };
    let content_hash = entry.content_hash.clone();
    let result = match entry.kind.as_str() {
        crate::history::kind::TEXT => {
            let text = entry.text.ok_or_else(|| ErrDto::new("history_missing"))?;
            clipboard::write_text(text)
                .await
                .map_err(|e| ErrDto::with("clipboard_write_failed", e))
        }
        crate::history::kind::IMAGE => {
            let hash = entry
                .blob_hash
                .ok_or_else(|| ErrDto::new("history_missing"))?;
            // The disk read plus the PNG decode block, so they move off the
            // async context
            let (width, height, rgba) =
                tauri::async_runtime::spawn_blocking(move || history.load_image_rgba(&hash))
                    .await
                    .map_err(|e| ErrDto::with("history_missing", e))?
                    .map_err(|e| ErrDto::with("history_missing", e))?;
            clipboard::write_image(width, height, rgba)
                .await
                .map_err(|e| ErrDto::with("clipboard_write_failed", e))
        }
        crate::history::kind::FILES => {
            let files = entry.files.ok_or_else(|| ErrDto::new("history_missing"))?;
            // Lazy validation: an entry whose source files were deleted or
            // moved is only reported stale once it is chosen
            if !files.iter().all(|p| p.exists()) {
                return Err(ErrDto::new("files_missing"));
            }
            clipboard::write_files(files)
                .await
                .map_err(|e| ErrDto::with("clipboard_write_failed", e))
        }
        _ => Err(ErrDto::new("history_missing")),
    };
    // Register the restore write: when the pump sees a change carrying this
    // hash it skips source application capture, so the bump keeps the original
    // source. Focus has already gone back to the paste target by then, and
    // capturing would rewrite the entry's source application to the paste
    // target rather than the real copy origin
    if result.is_ok() {
        *lock(&restore_hash) = Some(content_hash);
        // Auto-paste (off by default): the content is on the clipboard now, so
        // dismiss the panel and send the paste key on to whatever had focus.
        // Both restore paths come through here, which is what gives the slot
        // hotkeys the same behaviour as picking an entry in the panel
        let auto_paste = lock(&app.state::<AppState>().settings).auto_paste;
        if auto_paste {
            crate::autopaste::paste_after_dismiss(app);
        }
    }
    result
}

/// Whether auto-paste can work on this machine
///
/// `supported` is about the platform, `permitted` about the operating system
/// permission — the settings page needs them apart to tell "your system cannot
/// do this" from "grant the permission".
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoPasteStatus {
    /// Whether this platform can synthesize the keystroke at all
    pub supported: bool,
    /// Whether the operating system currently allows it
    pub permitted: bool,
}

impl AutoPasteStatus {
    /// Reads the current state
    fn current() -> Self {
        Self {
            supported: crate::autopaste::is_supported(),
            permitted: crate::autopaste::is_permitted(),
        }
    }
}

/// Reports whether auto-paste can work (the settings page asks on open)
#[tauri::command]
pub fn auto_paste_status() -> AutoPasteStatus {
    AutoPasteStatus::current()
}

/// Asks for the permission auto-paste needs and reports the state after
///
/// On macOS this raises the system authorization prompt; a grant only reaches
/// the process after a restart, so `permitted` staying false here is the
/// normal outcome rather than a failure.
#[tauri::command]
pub fn request_auto_paste_permission() -> AutoPasteStatus {
    crate::autopaste::request_permission();
    AutoPasteStatus::current()
}

/// Display PNG for an image entry, rendered directly by the preview card's
/// `<img>`
///
/// Binary IPC (`ipc::Response`) avoids base64 inflation; an image whose long
/// edge exceeds the cap is downsampled on the Rust side before transfer (see
/// `HistoryStore::preview_png`), taking a 5K screenshot from ~4MB down to a few
/// hundred KB and cutting WebView decode memory with it. The restore path is
/// unaffected: the blob keeps the original image.
#[tauri::command]
pub async fn history_image_png(
    app: tauri::AppHandle,
    id: String,
) -> Result<tauri::ipc::Response, ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        let entry = history.entry(&id).ok_or(ErrDto::new("history_missing"))?;
        let hash = entry.blob_hash.ok_or(ErrDto::new("history_missing"))?;
        history
            .preview_png(&hash)
            .map_err(|e| ErrDto::with("history_missing", e))
    })
    .await
    .map_err(|e| ErrDto::with("engine", e))??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Source application icon PNG for the preview card; returns an error when it
/// is not cached, and the frontend then hides the icon
#[tauri::command]
pub async fn app_icon_png(
    app: tauri::AppHandle,
    name: String,
) -> Result<tauri::ipc::Response, ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        history
            .app_icon_png(&name)
            .map_err(|e| ErrDto::with("history_missing", e))
    })
    .await
    .map_err(|e| ErrDto::with("engine", e))??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Show the preview card; the card calls this itself once it has measured its
/// content. Placement and the focus protection live in lib.rs.
///
/// `anchor_y`: the highlighted row's y in logical pixels relative to the top of
/// the panel document, so the card lines up with the row;
/// `height`: the content's actual height in logical pixels, which the window
/// sizes itself to
#[tauri::command]
pub fn show_preview(app: tauri::AppHandle, anchor_y: Option<f64>, height: Option<f64>) {
    crate::show_preview_impl(&app, anchor_y, height);
}

/// Hide the preview card; the frontend calls this when the highlighted row goes
/// away or the search has no match
#[tauri::command]
pub fn hide_preview(app: tauri::AppHandle) {
    crate::hide_preview_impl(&app);
}

/// Open the settings window, reached from the "Preferences" item in the history
/// panel's footer menu
#[tauri::command]
pub fn show_settings_window(app: tauri::AppHandle) {
    crate::show_main_window(&app);
}

/// Backdrop a window paints before its document has rendered
///
/// `PageLoadEvent::Finished` fires when the document has loaded, **not when it
/// has painted**, so showing the window then still puts an unpainted surface
/// on screen for a frame or two. Left at its default that surface is black,
/// and the about window visibly went black then light on Windows. Filling it
/// with the theme backdrop up front makes the same gap invisible.
///
/// The two colours match the inline anti-flash script in index.html and have
/// to stay in step with it. The theme comes from a live window: the frontend
/// pushes the user's preference onto the native windows through setTheme, so
/// this follows an explicit light/dark override as well as the system.
pub fn theme_backdrop(app: &tauri::AppHandle) -> tauri::window::Color {
    let dark = app
        .get_webview_window("main")
        .and_then(|window| window.theme().ok())
        .is_none_or(|theme| theme == tauri::Theme::Dark);
    if dark {
        tauri::window::Color(0x10, 0x14, 0x25, 0xff)
    } else {
        tauri::window::Color(0xee, 0xf1, 0xfa, 0xff)
    }
}

/// Open the about window, shared by the panel footer menu and the Linux tray
/// menu
///
/// A small standalone window, created **lazily**: statically configured windows
/// are built at startup, and keeping a WebView resident for a rarely used window
/// is not worth it. The first call creates it, closing it only hides it (handled
/// by the global CloseRequested handler), and opening it again just shows the
/// existing one.
///
/// An async command: on Windows, creating a webview inside a synchronous command
/// (on the main thread) risks a deadlock, as the Tauri docs state; issuing it
/// from a runtime thread lets Tauri schedule it onto the main thread itself.
#[tauri::command]
pub async fn show_about(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("about") {
        // The theme may have changed since this window was built, and the
        // backdrop is only read at creation — refresh it or a reopen flashes
        // the theme the window was created with
        let _ = win.set_background_color(Some(theme_backdrop(&app)));
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    // A fixed-size small window that keeps the system title bar. Created hidden
    // and shown only once the page has loaded: making it visible up front
    // flashes a frame of empty document first
    let built = tauri::WebviewWindowBuilder::new(
        &app,
        "about",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("Lanecho")
    .inner_size(300.0, 322.0)
    .resizable(false)
    .skip_taskbar(true)
    .center()
    .background_color(theme_backdrop(&app))
    .visible(false)
    .on_page_load(|window, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            let _ = window.show();
            let _ = window.set_focus();
        }
    })
    .build();
    if let Err(e) = built {
        tracing::warn!("Failed to create about window: {e}");
        // Race fallback for concurrent calls: another call just created it, so
        // show the existing window
        if let Some(win) = app.get_webview_window("about") {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

/// Application version for the about page; reads the version baked in at
/// packaging time, which comes from the same source as package.json
#[tauri::command]
pub fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Quit the application from the panel's footer menu; goes through the normal
/// exit path, where RunEvent::Exit performs the engine's graceful shutdown
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Fit the settings window's height to its content; the frontend calls this
/// once it has measured the current category page
///
/// `content_height`: the height the webview content needs, in logical pixels.
/// Chrome height and the bounds are worked out on the Rust side; see
/// [`crate::resize_settings_window_impl`]
#[tauri::command]
pub fn resize_settings_window(app: tauri::AppHandle, content_height: f64) {
    crate::resize_settings_window_impl(&app, content_height);
}

/// Delete a single history entry
///
/// async plus a blocking thread: synchronous commands run on the main event
/// loop thread, and deleting from disk there (blobs reach 16MB) freezes the UI
/// of every window.
#[tauri::command]
pub async fn delete_history_entry(app: tauri::AppHandle, id: String) -> Result<(), ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    let deleted = tauri::async_runtime::spawn_blocking(move || history.delete(&id))
        .await
        .map_err(|e| ErrDto::with("engine", e))?;
    if deleted {
        use tauri::Emitter;
        let _ = app.emit(events::HISTORY_CHANGED, ());
    }
    Ok(())
}

/// Clear all history, pinned entries included
///
/// The blobs directory tops out around 200×16MB ≈ 3.2GB; deleting that from a
/// synchronous command on the main thread freezes both windows for seconds, so
/// it has to run on a blocking thread.
#[tauri::command]
pub async fn clear_history(app: tauri::AppHandle) -> Result<(), ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    tauri::async_runtime::spawn_blocking(move || history.clear())
        .await
        .map_err(|e| ErrDto::with("engine", e))?;
    use tauri::Emitter;
    let _ = app.emit(events::HISTORY_CHANGED, ());
    Ok(())
}

/// Pin or unpin a history entry
#[tauri::command]
pub fn pin_history_entry(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    pinned: bool,
) {
    if state.history.set_pinned(&id, pinned) {
        use tauri::Emitter;
        let _ = app.emit(events::HISTORY_CHANGED, ());
    }
}

/// Bytes of disk the history occupies, shown on the settings page
///
/// The blobs part comes from a runtime counter (O(1)), leaving one metadata
/// query for the index. It still runs on a blocking thread: the frontend
/// triggers this after every copy, and the main thread does no fs work
#[tauri::command]
pub async fn history_usage(app: tauri::AppHandle) -> Result<u64, ErrDto> {
    let history = std::sync::Arc::clone(&app.state::<AppState>().history);
    tauri::async_runtime::spawn_blocking(move || history.disk_usage())
        .await
        .map_err(|e| ErrDto::with("engine", e))
}

/// Toggle incognito mode, which pauses history recording; session-scoped and
/// not persisted
///
/// There is a single entry point, the sync section of the settings page. The
/// event broadcast stays: other windows that want to mirror the incognito state
/// listen for INCOGNITO_STATE instead of polling
#[tauri::command]
pub fn set_incognito(app: tauri::AppHandle, on: bool) {
    let state = app.state::<AppState>();
    state
        .incognito
        .store(on, std::sync::atomic::Ordering::Relaxed);
    // Leaving incognito resumes recording: reset the watcher's dedup baseline
    // so content copied while incognito is recorded again on the next copy
    // (same cause and same fix as re-enabling a record type toggle)
    if !on {
        state
            .reset_dedupe
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    use tauri::Emitter;
    let _ = app.emit(events::INCOGNITO_STATE, on);
}

/// Current incognito state
#[tauri::command]
pub fn get_incognito(state: State<'_, AppState>) -> bool {
    state.incognito.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the panel's vibrancy material is active; the frontend switches its
/// translucent background variables on this
///
/// Configured windowEffects give no confirmation, so this is decided by platform
/// capability: always available on macOS (10.15 baseline); other platforms
/// declare no material in the config and keep an opaque background, which stops
/// a transparent window from showing the desktop through.
#[tauri::command]
pub fn window_effects_active() -> bool {
    cfg!(target_os = "macos")
}

/// Slot hotkeys that failed to register (the N in Alt+N); the settings page
/// uses this to flag which slots are taken
#[tauri::command]
pub fn get_slot_hotkey_failures(state: State<'_, AppState>) -> Vec<u8> {
    lock(&state.slot_hotkey_failures).clone()
}
