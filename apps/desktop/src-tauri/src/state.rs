//! Global application state (held by tauri manage) and the lock helper.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lanecho_core::sync::SyncEngine;

use crate::history::HistoryStore;
use crate::settings::Settings;

/// Global application state
pub struct AppState {
    /// Sync engine handle (pairing / syncing / toggles / shutdown; Arc because
    /// the event pump holds one to cancel echo registrations)
    pub engine: Arc<SyncEngine>,
    /// User settings (Arc because the event pump closure holds one; it does
    /// not go through app.state, which keeps it clear of injection timing)
    pub settings: Arc<Mutex<Settings>>,
    /// Clipboard history (Arc: shared by the event pump and the hotkey handler)
    pub history: Arc<HistoryStore>,
    /// Incognito mode, pausing history recording; session-scoped and not
    /// persisted, because resuming recording on restart is the safe default
    pub incognito: Arc<AtomicBool>,
    /// Whether the watcher should read image pixels (Arc because the clipboard
    /// watcher task holds one). This is the union of the two image switches,
    /// history recording and image sync: either consumer alone requires the
    /// pixels, so the read is skipped only when both are off. When skipped the
    /// watcher still emits a contentless event to advance the LWW baseline.
    /// Flipped once save_settings has persisted successfully
    pub record_images: Arc<AtomicBool>,
    /// Reset channel for the watcher's dedup baseline (Arc because the watcher
    /// task holds one): set when recording resumes (a type toggle is ticked
    /// back on, or incognito is left) and the watcher clears last_hash on its
    /// next tick; otherwise content copied while paused stays swallowed by
    /// dedup forever
    pub reset_dedupe: Arc<AtomicBool>,
    /// Slot hotkeys that failed to register (the N in Alt+N), recorded when
    /// another application already owns them; the settings page flags them,
    /// otherwise the toggle reads as on while the feature looks broken
    pub slot_hotkey_failures: Mutex<Vec<u8>>,
    /// Parsed global hotkeys, cached when apply_hotkeys registers them: the
    /// dispatch handler compares against these on every key press instead of
    /// re-parsing seven shortcut strings
    pub hotkeys: Mutex<ParsedHotkeys>,
    /// Content hash of the most recent history restore write (Arc because the
    /// event pump holds one): when the pump sees a LocalCopied carrying this
    /// hash it skips source application capture; focus has already gone back
    /// to the paste target by then, and capturing would wipe the entry's
    /// original source application (this pairs with the bump retention rule)
    pub restore_hash: Arc<Mutex<Option<String>>>,
    /// Compiled ignore rules (Arc because the ingest hop between watcher and
    /// engine holds one); rebuilt by save_settings when the ignore object
    /// changes
    pub ignore_rules: Arc<Mutex<crate::ignore::IgnoreRules>>,
    /// Whether native window effects are active on the floating windows.
    /// macOS is config-level vibrancy and reports true unconditionally; on
    /// Windows setup sets this once the Win11 backdrop has landed (never on
    /// older builds, so the frontend keeps the opaque palette); Linux never
    #[cfg_attr(
        not(windows),
        expect(
            dead_code,
            reason = "仅 Windows 的 setup 与查询命令读取; 其余平台按常量回答"
        )
    )]
    pub window_effects: AtomicBool,
    /// Engine data directory, where settings.json / identity.json /
    /// paired.json live
    pub data_dir: PathBuf,
    /// Sender and task handle of the history worker, drained in order on exit
    ///
    /// Exit must **drain the worker before flushing the index**: the image
    /// branch of `record` has an await between "the blob is on disk" and "the
    /// entry is in the in-memory table", so an index.json flushed at that
    /// instant lacks the entry while its blob is already on disk. The orphan
    /// sweep on the next startup then deletes it, which from the user's side
    /// is "the screenshot I just copied vanished".
    pub history_worker: Mutex<Option<HistoryWorker>>,
}

/// Shutdown handle for the history worker
pub struct HistoryWorker {
    /// Sender; dropping it is what lets the worker see the channel close and
    /// wrap up
    pub tx: tokio::sync::mpsc::Sender<crate::bridge::HistoryJob>,
    /// Task handle; awaiting it waits for the queue to drain
    pub handle: tauri::async_runtime::JoinHandle<()>,
}

/// Snapshot of the parsed global hotkeys, updated in step with what is
/// actually registered
#[derive(Default)]
pub struct ParsedHotkeys {
    /// Panel key; None when unset or when parsing failed
    pub panel: Option<tauri_plugin_global_shortcut::Shortcut>,
    /// Slot keys and their slot numbers; only the ones that registered, empty
    /// when slots are disabled
    pub slots: Vec<(tauri_plugin_global_shortcut::Shortcut, u8)>,
}

/// Take the lock, recovering the inner data from a poisoned mutex so that one
/// panic does not poison the global locks in a chain
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
