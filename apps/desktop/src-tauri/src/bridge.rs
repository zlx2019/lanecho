//! Engine bridge: starts the SyncEngine and pumps EngineEvent into Tauri
//! events.
//!
//! Shell layer responsibilities:
//! - Owns system clipboard watching (spawn_watcher → engine)
//! - Lands remote syncs: ApplyRemote → write the system clipboard + optional
//!   notification
//! - Forwards events: engine events → frontend Tauri events (names listed in
//!   [`events`])

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;

use lanecho_core::DEFAULT_DISCOVERY_PORT;
use lanecho_core::clipboard::{self, ClipboardContent, ClipboardEvent, now_ms};
use lanecho_core::protocol::PeerInfo;
use lanecho_core::sync::{EngineConfig, EngineEvent, SyncEngine};

use crate::history::{HistoryConfig, HistoryStore};
use crate::locale;
use crate::settings::Settings;
use crate::state::{AppState, lock};

/// Frontend event names (**a rename must be applied on both sides: here and
/// the frontend's src/events.ts**)
pub mod events {
    /// Peer came online or updated its info, payload: PeerDto
    pub const PEER_UP: &str = "peer-up";
    /// Peer went offline, payload: fingerprint string
    pub const PEER_DOWN: &str = "peer-down";
    /// Incoming pairing request, payload: PeerDto
    pub const PAIR_REQUESTED: &str = "pair-requested";
    /// Pairing established, payload: PeerDto
    pub const PAIRED: &str = "paired";
    /// Pairing removed, payload: fingerprint string
    pub const UNPAIRED: &str = "unpaired";
    /// Remote clipboard applied, payload: SyncedDto
    pub const CLIPBOARD_SYNCED: &str = "clipboard-synced";
    /// Sync direction policy changed (tray toggle / settings save echo),
    /// payload: syncMode string. Not a bool: the frontend maps a bool back
    /// coarsely to both/off, which would overwrite send/receive.
    pub const SYNC_STATE: &str = "sync-state-changed";
    /// History changed (added / count bumped / deleted / cleared), used to
    /// refresh the panel and the settings page, no payload
    pub const HISTORY_CHANGED: &str = "history-changed";
    /// Incognito mode changed (tray toggle echo), payload: bool
    pub const INCOGNITO_STATE: &str = "incognito-changed";
}

/// Peer info DTO (shared by the peer-up / pair-requested / paired events and
/// the device list)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerDto {
    /// Device ID
    pub device_id: String,
    /// Display name
    pub name: String,
    /// Certificate fingerprint
    pub fingerprint: String,
    /// Platform identifier
    pub platform: String,
    /// OS version description
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

/// Remote sync event DTO
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncedDto {
    /// Source device name
    pub from_name: String,
    /// Content preview (first line, truncated, for display)
    pub preview: String,
    /// Time it was applied (Unix milliseconds)
    pub at: u64,
}

/// Starts the engine and wires up the clipboard and the event pump; setup
/// waits on this synchronously through block_on, so AppState is ready by the
/// time the first command arrives
pub async fn start_engine(app: tauri::AppHandle, data_dir: PathBuf) -> anyhow::Result<AppState> {
    let settings = Settings::load(&data_dir);
    let settings_shared = Arc::new(Mutex::new(settings.clone()));

    // The history load (disk read + orphan blob sweep) and the engine start
    // (port bind / mDNS) are independent: running them in parallel shortens
    // the startup critical path, with the blocking IO on a blocking thread
    let history_task = tauri::async_runtime::spawn_blocking({
        let data_dir = data_dir.clone();
        move || HistoryStore::load(&data_dir)
    });
    let (clip_tx, clip_rx) = mpsc::channel(16);
    let (engine, events_rx) = SyncEngine::start(
        EngineConfig {
            data_dir: data_dir.clone(),
            tcp_port: settings.tcp_port,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            passive: false,
            sync_mode: crate::sync_mode_of(&settings),
            sync_types: crate::sync_types_of(&settings),
            max_sync_file_bytes: crate::max_sync_file_bytes_of(&settings),
        },
        clip_rx,
    )
    .await?;

    // Take over system clipboard watching: local copy → engine (change-stamp
    // polling). The skip-the-read flag is the **conjunction of the history
    // and the sync toggle**: with "record images" off but "sync images" on
    // the watcher must still read pixels; only both off skips the read.
    // save_settings recomputes this combined value live
    let record_images = Arc::new(AtomicBool::new(
        settings.history_record_images || settings.sync_images,
    ));
    let reset_dedupe = Arc::new(AtomicBool::new(false));
    // The watcher feeds the ingest hop rather than the engine directly: the
    // ignore rules are evaluated in between (the Rust twin of the native
    // client's AppCore.ingest). restore_hash is created up here because the
    // ingest reads it to exempt restore writes from the application rule.
    let restore_hash = Arc::new(Mutex::new(None));
    let ignore_rules = Arc::new(Mutex::new(crate::ignore::IgnoreRules::new(
        &settings.ignore,
    )));
    let (watch_tx, watch_rx) = mpsc::channel(16);
    clipboard::spawn_watcher(
        watch_tx,
        Arc::clone(&record_images),
        Arc::clone(&reset_dedupe),
    );
    spawn_ingest(
        watch_rx,
        clip_tx,
        Arc::clone(&ignore_rules),
        Arc::clone(&restore_hash),
    );
    let engine = Arc::new(engine);
    let history = Arc::new(
        history_task
            .await
            .map_err(|e| anyhow::anyhow!("History storage task was interrupted: {e}"))?,
    );
    // Sweep orphan sync-files at startup (blocking IO, kept off the startup
    // critical path)
    {
        let history = Arc::clone(&history);
        tauri::async_runtime::spawn_blocking(move || history.sweep_orphan_sync_files());
    }
    let incognito = Arc::new(AtomicBool::new(false));
    let (history_tx, history_worker) = spawn_history_worker(
        app.clone(),
        Arc::clone(&history),
        Arc::clone(&settings_shared),
    );
    // On exit this sole strong sender closes the channel and waits for the
    // worker to drain (see AppState::history_worker)
    let history_worker = crate::state::HistoryWorker {
        tx: history_tx.clone(),
        handle: history_worker,
    };
    spawn_event_pump(PumpDeps {
        app,
        events_rx,
        settings: Arc::clone(&settings_shared),
        engine: Arc::clone(&engine),
        history: Arc::clone(&history),
        history_tx: history_tx.downgrade(),
        incognito: Arc::clone(&incognito),
        restore_hash: Arc::clone(&restore_hash),
    });

    Ok(AppState {
        engine,
        settings: settings_shared,
        history,
        incognito,
        record_images,
        reset_dedupe,
        slot_hotkey_failures: Mutex::new(Vec::new()),
        hotkeys: Mutex::new(crate::state::ParsedHotkeys::default()),
        restore_hash,
        ignore_rules,
        data_dir,
        history_worker: Mutex::new(Some(history_worker)),
    })
}

/// Watcher → engine, with the ignore rules evaluated in between
///
/// ImageUnread passes straight through (contentless, nothing to judge). A
/// restore write (hash matching the registration) exempts the application
/// rule only — the content does not come from whatever happens to be
/// frontmost — while regex/type/file rules still apply, restoring being
/// equivalent to copying. The registration itself is NOT consumed here: the
/// pump still needs it to preserve the original source application.
fn spawn_ingest(
    mut watch_rx: mpsc::Receiver<ClipboardEvent>,
    clip_tx: mpsc::Sender<ClipboardEvent>,
    rules: Arc<Mutex<crate::ignore::IgnoreRules>>,
    restore_hash: Arc<Mutex<Option<String>>>,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(mut event) = watch_rx.recv().await {
            if !matches!(event.content, ClipboardContent::ImageUnread) {
                let restored = lock(&restore_hash)
                    .as_deref()
                    .is_some_and(|registered| registered == event.hash);
                // The frontmost query is a system call: made outside the
                // rules lock, and only when some application rule could match
                let source_app_id = if restored || !lock(&rules).wants_source_app() {
                    None
                } else {
                    clipboard::frontapp::frontmost_app_id()
                };
                let verdict = lock(&rules).evaluate(
                    &event.content,
                    &event.pasteboard_types,
                    source_app_id.as_deref(),
                );
                event.suppress_broadcast = verdict.suppress_sync;
                event.suppress_record = verdict.suppress_record;
            }
            if clip_tx.send(event).await.is_err() {
                return;
            }
        }
    });
}

/// Dependencies of the event pump (all captured as Arc rather than reached
/// through app.state, which sidesteps the state injection ordering)
struct PumpDeps {
    app: tauri::AppHandle,
    events_rx: mpsc::Receiver<EngineEvent>,
    settings: Arc<Mutex<Settings>>,
    engine: Arc<SyncEngine>,
    /// History store handle (the pump only uses it to check the icon disk
    /// cache; recording still goes through history_tx)
    history: Arc<HistoryStore>,
    /// A **weak** sender for the history worker: the only strong sender lives
    /// in AppState, and dropping it during the exit drain closes the channel.
    /// The pump must not hold a strong one — its event stream never ends
    /// before the process dies (the sending end lives inside SyncEngine), so
    /// the channel would never close and every exit would wait out the full
    /// drain timeout.
    history_tx: mpsc::WeakSender<HistoryJob>,
    incognito: Arc<AtomicBool>,
    /// Hash of the most recent history restore write (registered on the
    /// commands side, see the AppState field of the same name)
    restore_hash: Arc<Mutex<Option<String>>>,
}

/// A history record job (pump → worker)
pub struct HistoryJob {
    content: ClipboardContent,
    hash: String,
    at: u64,
    origin: Option<String>,
    /// Frontmost application name at the moment of the local copy (shown on
    /// the preview card; None for remote entries)
    source_app: Option<String>,
    /// Frontmost application icon as PNG (captured at most once per
    /// application per session, the worker writes it to the cache)
    source_app_icon: Option<Vec<u8>>,
}

/// Takes a history config snapshot from the settings
fn history_config(settings: &Mutex<Settings>) -> HistoryConfig {
    let s = lock(settings);
    HistoryConfig {
        max_entries: s.history_max_entries,
        record_text: s.history_record_text,
        record_images: s.history_record_images,
        record_files: s.history_record_files,
    }
}

/// A dedicated serialized worker for history records: it moves record (which
/// includes PNG encoding of large images and can take seconds) off the single
/// event pump, so it cannot head-of-line block realtime events such as
/// ApplyRemote or the pairing dialog; one task consuming in order keeps the
/// dedup lookup and insert from running concurrently.
fn spawn_history_worker(
    app: tauri::AppHandle,
    history: Arc<HistoryStore>,
    settings: Arc<Mutex<Settings>>,
) -> (
    mpsc::Sender<HistoryJob>,
    tauri::async_runtime::JoinHandle<()>,
) {
    let (tx, mut rx) = mpsc::channel::<HistoryJob>(32);
    let handle = tauri::async_runtime::spawn(async move {
        while let Some(job) = rx.recv().await {
            let HistoryJob {
                content,
                hash,
                at,
                origin,
                source_app,
                source_app_icon,
            } = job;
            // Cache the source application icon (disk write on a blocking
            // thread; inside the serialized worker, off the critical path)
            if let (Some(name), Some(png)) = (source_app.clone(), source_app_icon) {
                let icons = Arc::clone(&history);
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    icons.save_app_icon(&name, &png);
                })
                .await;
            }
            let outcome = history
                .record(
                    content,
                    &hash,
                    at,
                    origin,
                    source_app,
                    history_config(&settings),
                )
                .await;
            if outcome != crate::history::RecordOutcome::Skipped {
                emit(&app, events::HISTORY_CHANGED, ());
            }
        }
    });
    (tx, handle)
}

/// The single event pump: EngineEvent → clipboard write / history record /
/// system notification / Tauri event
fn spawn_event_pump(deps: PumpDeps) {
    let PumpDeps {
        app,
        mut events_rx,
        settings,
        engine,
        history,
        history_tx,
        incognito,
        restore_hash,
    } = deps;
    tauri::async_runtime::spawn(async move {
        // Application names whose icon was already captured this session
        // (owned by the single pump task, so no lock is needed)
        let mut known_icons: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                    crate::update_pending_tooltip(&app);
                    emit(&app, events::PAIR_REQUESTED, PeerDto::from(&peer));
                }
                EngineEvent::Paired { peer } => {
                    crate::update_pending_tooltip(&app);
                    emit(&app, events::PAIRED, PeerDto::from(&peer));
                }
                EngineEvent::Unpaired { fingerprint } => {
                    emit(&app, events::UNPAIRED, fingerprint);
                }
                // Local copy → history worker (recording pauses in incognito
                // mode)
                EngineEvent::LocalCopied {
                    content,
                    hash,
                    timestamp_ms,
                    suppress_record,
                } => {
                    if incognito.load(Ordering::Relaxed) {
                        continue;
                    }
                    // A skipped-read image event exists only to advance the
                    // engine's LWW baseline, so it stops here — going any
                    // further would query the frontmost application (an
                    // AppKit call) for nothing and record would reject it
                    if matches!(content, ClipboardContent::ImageUnread) {
                        continue;
                    }
                    // Source application: the copy happened within one poll
                    // interval (≤250ms), so the current frontmost application
                    // is the source (frontapp already wraps its calls in an
                    // autoreleasepool as required). Exception: skip the
                    // capture when this change is a history restore write (a
                    // hash matching the registration) — focus has been handed
                    // back to the target application by then and capturing
                    // would overwrite the original source. A registration
                    // that does not match is discarded as well: the user has
                    // copied something else since.
                    let restored = lock(&restore_hash)
                        .take()
                        .is_some_and(|restored_hash| restored_hash == hash);
                    // Ignore rules: the record leg is cut, sync already went
                    // its way. Placed after the restore registration is
                    // consumed — an ignored restore must not leave it behind
                    // to mislabel the next genuine copy
                    if suppress_record {
                        continue;
                    }
                    let source_app = if restored {
                        None
                    } else {
                        clipboard::frontapp::frontmost_app_name()
                    };
                    // The icon is captured alongside it, at most once per
                    // application per session (TIFF expansion costs
                    // milliseconds and does not belong on a hot path); when
                    // the disk cache already holds the icon even the
                    // expansion is skipped, so each application is really
                    // expanded only once per install (one stat, on the first
                    // sighting of a session)
                    let source_app_icon = match &source_app {
                        Some(name) if !known_icons.contains(name) => {
                            known_icons.insert(name.clone());
                            if history.has_app_icon(name) {
                                None
                            } else {
                                clipboard::frontapp::frontmost_app_icon_png()
                            }
                        }
                        _ => None,
                    };
                    send_history_job(
                        &history_tx,
                        HistoryJob {
                            content,
                            hash,
                            at: timestamp_ms,
                            origin: None,
                            source_app,
                            source_app_icon,
                        },
                    );
                }
                EngineEvent::ApplyRemote {
                    content,
                    from,
                    hash,
                    ..
                } => {
                    // The notification preview depends on the content type:
                    // first line of text / image dimensions / file name
                    let preview = match &content {
                        ClipboardContent::Text(text) => crate::history::preview_text(text),
                        ClipboardContent::Image { width, height, .. } => {
                            format!("[{} {width}×{height}]", locale_image_word(&app))
                        }
                        ClipboardContent::Files(paths) => files_preview(paths),
                        other => other.kind().to_string(),
                    };
                    // The shell layer lands it: write to the system clipboard
                    // (the echo hash is already registered on the engine
                    // side). On failure the echo registration must be
                    // cancelled, or the orphan hash swallows the next genuine
                    // local copy of the same content (the ApplyRemote
                    // contract)
                    let write_result = match content.clone() {
                        ClipboardContent::Text(text) => clipboard::write_text(text).await,
                        ClipboardContent::Image {
                            width,
                            height,
                            rgba,
                        } => clipboard::write_image(width, height, rgba).await,
                        ClipboardContent::Files(paths) => clipboard::write_files(paths).await,
                        other => {
                            tracing::warn!(
                                kind = other.kind(),
                                "Unknown remote content type; skipping"
                            );
                            engine.cancel_echo(&hash);
                            continue;
                        }
                    };
                    if let Err(e) = write_result {
                        tracing::warn!(
                            "Failed to write remote sync content to system clipboard: {e}"
                        );
                        engine.cancel_echo(&hash);
                        continue;
                    }
                    // Remote writes go into the history too (origin = source
                    // device name); the engine swallows the echo event so it
                    // never reaches LocalCopied, making this the only entry
                    // point
                    if !incognito.load(Ordering::Relaxed) {
                        send_history_job(
                            &history_tx,
                            HistoryJob {
                                content,
                                hash,
                                at: now_ms(),
                                origin: Some(from.name.clone()),
                                source_app: None,
                                source_app_icon: None,
                            },
                        );
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
                    Ok(()) => tracing::debug!(to = %to.name, "Clipboard synced to peer"),
                    Err(e) => tracing::info!(to = %to.name, "Sync failed: {e}"),
                },
            }
        }
    });
}

/// Submits a history record job: drop it with a warning when the queue is
/// full, **never await**
///
/// The event pump is the only consumer of engine events, so blocking here
/// propagates backpressure through the engine event channel all the way to
/// discovery (whose full channel really does drop PeerDown, leaving devices
/// falsely online) and to inbound sessions. Losing one history record under
/// an extreme burst is far more acceptable than stalling realtime events —
/// that is what actually makes good on "record is not on the critical path".
fn send_history_job(history_tx: &mpsc::WeakSender<HistoryJob>, job: HistoryJob) {
    // A failed upgrade means the drain already closed the channel (the
    // process is exiting), so dropping silently is fine
    let Some(tx) = history_tx.upgrade() else {
        return;
    };
    if let Err(e) = tx.try_send(job) {
        tracing::warn!("History queue is full; dropping one entry: {e}");
    }
}

/// Emits a Tauri event; a failure is only logged and does not affect the
/// engine
fn emit<T: Serialize + Clone>(app: &tauri::AppHandle, event: &str, payload: T) {
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!("Failed to emit frontend event {event}: {e}");
    }
}

/// Sends a system notification when the application is not in the foreground
/// (with any window focused it is already visible in-app, so do not interrupt)
///
/// The word "image" used in notification previews, in the current language
fn locale_image_word(app: &tauri::AppHandle) -> &'static str {
    crate::locale::current(app).image_word
}

/// Notification preview for a batch of files: first file name + count
fn files_preview(paths: &[std::path::PathBuf]) -> String {
    let first = paths
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if paths.len() > 1 {
        format!("{first} +{}", paths.len() - 1)
    } else {
        first
    }
}

/// Walks every window: checking only main would still pop a notification
/// while the user is working in the history panel
pub fn notify_if_unfocused(app: &tauri::AppHandle, title: &str, body: &str) {
    use tauri::Manager;
    let focused = app
        .webview_windows()
        .values()
        .any(|w| w.is_focused().unwrap_or(false));
    if focused {
        return;
    }
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::debug!("Failed to send system notification: {e}");
    }
}
