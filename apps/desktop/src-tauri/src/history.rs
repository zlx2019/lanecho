//! Clipboard history engine.
//!
//! Storage layout (under the engine data directory):
//! - `history/index.json` — metadata for every entry plus inlined text
//!   (atomic write: tmp + rename)
//! - `history/blobs/<blake3>.png` — the image payloads, addressed by hash
//!
//! Semantics:
//! - Dedup: a content_hash hit only bumps copy_count and refreshes
//!   last_copied_at; no new entry is added
//! - Images map 1:1 onto content_hash (the hash derives from the pixels);
//!   deleting an entry deletes its blob
//! - Eviction: once over the cap, drop the unpinned entry with the oldest
//!   last_copied_at; pinned entries never take part in eviction
//! - File entries store path references only (the clipboard itself holds a
//!   reference); an entry goes stale once its source file is removed
//! - Text is inlined byte-for-byte (a hard rule of this repository);
//!   truncation and escaping happen only in the preview presentation layer

use std::collections::{HashSet, VecDeque};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fast_image_resize as fir;
use serde::{Deserialize, Serialize};

use lanecho_core::clipboard::ClipboardContent;

/// Index file name
const INDEX_FILE: &str = "index.json";
/// Directory holding the image payloads
const BLOBS_DIR: &str = "blobs";
/// Cache directory for source application icons (addressed by the hash of the
/// application name; not clipboard content, so `clear` leaves it alone)
const APP_ICONS_DIR: &str = "appicons";
/// Per-image cap, measured on the encoded PNG bytes
///
/// Raised from 10MB to 16MB: a screenshot with **busy** content encodes to
/// roughly 1.8 bytes/pixel (2560×1440 measures 6.7MB), so the old cap could
/// turn away even a full-screen 4K capture — and only after paying for the
/// encode. At that ratio 16MB covers about 8.8MP (4K 3840×2160 = 8.3MP); 5K
/// (14.7MP) only fits when the content is flat (pure UI, large areas of one
/// color), busy content is still rejected. The cap is the multiplier on the
/// worst-case footprint (× the retained entry count, 200 by default → 3.2GB
/// in theory), which is why it goes no higher; the real footprint is visible
/// on the storage page in settings
const MAX_IMAGE_PNG_BYTES: usize = 16 * 1024 * 1024;
/// Per-text cap: without one, a single huge text entry weighs down every
/// write to disk, every list, and every startup load, degrading the whole
/// history subsystem; anything over the cap is not recorded
const MAX_TEXT_BYTES: usize = 5 * 1024 * 1024;
/// Cap on the long edge (in pixels) of the image shown in the preview card
///
/// The card displays images at ≤330pt tall (frontend PreviewCard), so an
/// 800px long edge is enough to look right at 2x Retina; pushing a whole 5K
/// original over IPC (~4MB) just to have the WebView decode it into a ~56MB
/// bitmap is pure waste. **Applies to the presentation path only**: the blob
/// and the restore path always keep the original resolution
const PREVIEW_IMAGE_MAX_EDGE: u32 = 800;
/// Capacity (in entries) of the downsampled preview cache, matched to the
/// frontend Blob LRU (cap 12): once the frontend evicts, the next hover hits
/// here instead of redoing "decode original + resize + re-encode"
const PREVIEW_CACHE_CAP: usize = 12;

/// Content type constants (the `kind` field)
pub mod kind {
    /// Plain text
    pub const TEXT: &str = "text";
    /// Image
    pub const IMAGE: &str = "image";
    /// List of file references
    pub const FILES: &str = "files";
}

/// One history entry, doubling as the DTO (serialized camelCase)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryEntry {
    /// Stable ID (UUID)
    pub id: String,
    /// Content type (see [`kind`])
    pub kind: String,
    /// Text content, inlined byte-for-byte when kind=text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Image blob hash; when kind=image it points at blobs/<hash>.png
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_hash: Option<String>,
    /// File path references (kind=files)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PathBuf>>,
    /// Summary shown in the list (truncated first line of the text / image
    /// dimensions / list of file names)
    pub preview: String,
    /// Content hash: the dedup key, derived the same way as
    /// ClipboardContent::hash
    pub content_hash: String,
    /// When the content was first copied (Unix milliseconds)
    pub first_copied_at: u64,
    /// When the content was last copied (Unix milliseconds)
    pub last_copied_at: u64,
    /// Copy count, incremented every time the same content is copied again
    pub copy_count: u32,
    /// Origin: None = copied on this machine, Some(device name) = written in
    /// by a remote sync
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Source application: the frontmost application at the time of a local
    /// copy; None for remote entries and when the lookup fails
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app: Option<String>,
    /// Pinned to the top; takes no part in eviction
    pub pinned: bool,
}

impl Default for HistoryEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: kind::TEXT.to_string(),
            text: None,
            blob_hash: None,
            files: None,
            preview: String::new(),
            content_hash: String::new(),
            first_copied_at: 0,
            last_copied_at: 0,
            copy_count: 1,
            origin: None,
            source_app: None,
            pinned: false,
        }
    }
}

/// List projection DTO: shaped like [`HistoryEntry`] but **carries no inlined
/// full text**
///
/// The panel list refetches everything on every summon and on every
/// HISTORY_CHANGED, and sending the full text (capped at 5MB per entry) over
/// IPC alongside it is the single largest steady-state waste; a byte count
/// stands in for the text instead. The full text is fetched one entry at a
/// time through `entry_text`, and search runs on the storage side (see
/// [`HistoryStore::search`]).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntryMeta {
    /// Stable ID
    pub id: String,
    /// Content type (see [`kind`])
    pub kind: String,
    /// File path references (kind=files; the paths are the content itself and
    /// are small, so they ship with the list)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PathBuf>>,
    /// Summary shown in the list
    pub preview: String,
    /// When the content was first copied (Unix milliseconds)
    pub first_copied_at: u64,
    /// When the content was last copied (Unix milliseconds)
    pub last_copied_at: u64,
    /// Copy count
    pub copy_count: u32,
    /// Source device, when the entry was written in by a remote
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Source application, for a local copy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app: Option<String>,
    /// Pinned to the top
    pub pinned: bool,
    /// Text size in bytes (kind=text): the full text does not ship with the
    /// list, so this is what conveys its size
    pub text_len: usize,
}

impl HistoryEntryMeta {
    /// Project a stored entry, keeping only the byte count of the full text
    fn of(entry: &HistoryEntry) -> Self {
        Self {
            id: entry.id.clone(),
            kind: entry.kind.clone(),
            files: entry.files.clone(),
            preview: entry.preview.clone(),
            first_copied_at: entry.first_copied_at,
            last_copied_at: entry.last_copied_at,
            copy_count: entry.copy_count,
            origin: entry.origin.clone(),
            source_app: entry.source_app.clone(),
            pinned: entry.pinned,
            text_len: entry.text.as_deref().map_or(0, str::len),
        }
    }
}

/// List ordering comparator: pinned entries always come first; sort =
/// "frequent" orders by copy count, anything else by recency
/// —— shared by list / list_meta / entry_id_at, because the slot order and the
/// list order have to agree
fn compare(sort: &str, a: &HistoryEntry, b: &HistoryEntry) -> std::cmp::Ordering {
    if sort == "frequent" {
        b.pinned
            .cmp(&a.pinned)
            .then(b.copy_count.cmp(&a.copy_count))
            .then(b.last_copied_at.cmp(&a.last_copied_at))
    } else {
        b.pinned
            .cmp(&a.pinned)
            .then(b.last_copied_at.cmp(&a.last_copied_at))
    }
}

/// Snapshot of the type toggles and the capacity in force while recording,
/// taken from Settings
#[derive(Debug, Clone, Copy)]
pub struct HistoryConfig {
    /// Entry cap
    pub max_entries: usize,
    /// Record text
    pub record_text: bool,
    /// Record images
    pub record_images: bool,
    /// Record file references
    pub record_files: bool,
}

/// Outcome of a record call
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// A new entry was added
    Added,
    /// Hit an existing entry; only the count and timestamp were refreshed
    Bumped,
    /// Skipped by a type toggle or a size cap
    Skipped,
}

/// History storage: in-memory table plus persistence to disk
pub struct HistoryStore {
    /// History directory (<data_dir>/history)
    dir: PathBuf,
    /// Landing root for remote files (`<data_dir>/sync-files`, **not** under
    /// dir): it has to be the same root the engine writes to in net.rs,
    /// otherwise cascading deletes, the startup sweep, and the footprint
    /// statistics all spin against an empty directory
    sync_root: PathBuf,
    /// Entry table, in no particular order — the sort happens in list; the Arc
    /// lets the persist task take the latest snapshot while holding the lock
    entries: Arc<Mutex<Vec<HistoryEntry>>>,
    /// Serializes writes to disk: concurrent saves interleaving on the same
    /// temporary file produce a torn index.json, and load treats corruption as
    /// an empty table — silently dropping the entire history
    io_lock: Arc<Mutex<()>>,
    /// Marks that a writer is already queued: the writer takes the latest
    /// snapshot under the lock, so a burst of consecutive changes only needs
    /// one write to disk — queueing again is pure redundancy (N full writes to
    /// disk plus N blocking threads waiting)
    save_pending: Arc<std::sync::atomic::AtomicBool>,
    /// Running total of the bytes the blobs occupy (initialized by a single
    /// walk during load, adjusted on write and delete): the frontend triggers
    /// the footprint query after every copy, which is not worth a walk of the
    /// whole blobs directory each time
    blob_bytes: std::sync::atomic::AtomicU64,
    /// Cache of downsampled preview images (key = blob_hash; content-addressed
    /// so it never goes stale; a small LRU ring)
    preview_cache: Mutex<VecDeque<(String, Vec<u8>)>>,
    /// Clear generation, incremented on every clear
    ///
    /// The image branch of record yields at the PNG encode and the write to
    /// disk, and clear can cut in at those points; on resume the generation
    /// decides whether to discard the recording — otherwise "clear history"
    /// leaves behind an entry whose blob is already gone (a broken image), and
    /// the footprint count stays inflated until the next restart.
    clear_generation: std::sync::atomic::AtomicU64,
}

impl HistoryStore {
    /// Load from the data directory; missing or corrupt reads as an empty table
    pub fn load(data_dir: &Path) -> Self {
        let dir = data_dir.join("history");
        let entries: Vec<HistoryEntry> = std::fs::read(dir.join(INDEX_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // Orphan blob cleanup: writing a blob and writing the index are not
        // atomic, so a crash or a race with clear can leave a blob no entry
        // references (inflating the footprint and never reclaimed); sweep them
        // once at startup. Done synchronously during construction, so there is
        // no window where it races the first record.
        let referenced: HashSet<&str> = entries
            .iter()
            .filter_map(|e| e.blob_hash.as_deref())
            .collect();
        // The same walk seeds the blob footprint: from here on the footprint
        // query is an O(1) counter read instead of a directory walk (the
        // frontend triggers that query after every copy)
        let mut blob_bytes: u64 = 0;
        if let Ok(blobs) = std::fs::read_dir(dir.join(BLOBS_DIR)) {
            for file in blobs.flatten() {
                let name = file.file_name();
                if let Some(hash) = name.to_str().and_then(|n| n.strip_suffix(".png"))
                    && !referenced.contains(hash)
                {
                    tracing::info!(hash, "Removing orphaned image blob");
                    let _ = std::fs::remove_file(file.path());
                    continue;
                }
                blob_bytes += file.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        Self {
            dir,
            sync_root: data_dir.join(lanecho_core::sync::SYNC_FILES_DIR),
            entries: Arc::new(Mutex::new(entries)),
            io_lock: Arc::new(Mutex::new(())),
            save_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            blob_bytes: std::sync::atomic::AtomicU64::new(blob_bytes),
            preview_cache: Mutex::new(VecDeque::new()),
            clear_generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Take the lock, recovering from poisoning
    fn lock(&self) -> MutexGuard<'_, Vec<HistoryEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one piece of clipboard content; the event pump calls this
    /// serially
    ///
    /// **Calls to record** never overlap each other, but the encode and the
    /// write to disk in the image branch yield, and
    /// [`HistoryStore::clear`] (a separate task, from the `clear_history`
    /// command) can cut in there; [`HistoryStore::clear_generation`] is what
    /// discards the recording afterwards. Do not assume there is no
    /// concurrency window here.
    ///
    /// The PNG encode runs on a blocking thread outside the lock;
    /// `content_hash` comes from the caller (the watcher and the engine have
    /// already computed it, so large images are not hashed twice). Taken by
    /// value: the worker already owns the content, so the text and pixels move
    /// straight into the entry and the encode closure, saving one full copy of
    /// a large image (RGBA for a 4K screenshot peaks around 33MB).
    pub async fn record(
        &self,
        content: ClipboardContent,
        content_hash: &str,
        at: u64,
        origin: Option<String>,
        source_app: Option<String>,
        cfg: HistoryConfig,
    ) -> RecordOutcome {
        // Type toggles
        let enabled = match &content {
            ClipboardContent::Text(_) => cfg.record_text,
            ClipboardContent::Image { .. } => cfg.record_images,
            ClipboardContent::Files(_) => cfg.record_files,
            // This event only arises when record_images = false and the read
            // was skipped, so there is no content to record
            ClipboardContent::ImageUnread => false,
        };
        if !enabled {
            return RecordOutcome::Skipped;
        }
        // Dedup: a hit only bumps the count. Rules for refreshing the origin:
        // a remote write has an unambiguous origin (the device name, and the
        // source application is cleared); a local copy only overwrites when
        // the source application **was captured** — None (capture failed, a
        // restore wrote it, or we are the frontmost app) keeps the old value
        // rather than losing the original source
        {
            let mut entries = self.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.content_hash == content_hash) {
                entry.copy_count = entry.copy_count.saturating_add(1);
                entry.last_copied_at = at;
                if origin.is_some() {
                    entry.origin = origin;
                    entry.source_app = None;
                } else {
                    entry.origin = None;
                    if source_app.is_some() {
                        entry.source_app = source_app;
                    }
                }
                drop(entries);
                self.save();
                return RecordOutcome::Bumped;
            }
        }
        // New entry: images are encoded and written to disk outside the lock
        // first
        let mut entry = HistoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            content_hash: content_hash.to_string(),
            first_copied_at: at,
            last_copied_at: at,
            copy_count: 1,
            origin,
            source_app,
            pinned: false,
            ..Default::default()
        };
        match content {
            ClipboardContent::Text(text) => {
                if text.len() > MAX_TEXT_BYTES {
                    tracing::info!(
                        bytes = text.len(),
                        "Text exceeds per-entry history limit; skipping"
                    );
                    return RecordOutcome::Skipped;
                }
                entry.kind = kind::TEXT.to_string();
                entry.preview = preview_text(&text);
                entry.text = Some(text);
            }
            ClipboardContent::Image {
                width,
                height,
                rgba,
            } => {
                // Rough check before encoding: reject an extreme image
                // outright (>128MB of RGBA, roughly 5.7K square) so seconds
                // are not spent encoding something certain to exceed the cap
                // (the exact cap is still measured on the encoded PNG)
                if rgba.len() > 128 * 1024 * 1024 {
                    tracing::info!(
                        bytes = rgba.len(),
                        "Raw image data is too large; skipping history entry"
                    );
                    return RecordOutcome::Skipped;
                }
                // The encode and the write to disk are each a yield point (for
                // a 4K screenshot they add up to hundreds of milliseconds),
                // and the clear_history command can cut in from another task
                // meanwhile — note the generation and re-check on resume
                let generation = self
                    .clear_generation
                    .load(std::sync::atomic::Ordering::SeqCst);
                let png =
                    tauri::async_runtime::spawn_blocking(move || encode_png(width, height, &rgba))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                let Some(png) = png else {
                    tracing::warn!("Failed to encode history image as PNG; skipping");
                    return RecordOutcome::Skipped;
                };
                if png.len() > MAX_IMAGE_PNG_BYTES {
                    tracing::info!(
                        bytes = png.len(),
                        "History image exceeds per-entry limit; skipping"
                    );
                    return RecordOutcome::Skipped;
                }
                let png_len = png.len() as u64;
                let blobs_dir = self.dir.join(BLOBS_DIR);
                let path = self.blob_path(content_hash);
                let written = tauri::async_runtime::spawn_blocking(move || {
                    Self::write_blob(&blobs_dir, &path, &png)
                })
                .await;
                let freshly_written = match written {
                    Ok(Ok(written)) => written,
                    Ok(Err(e)) => {
                        tracing::warn!("Failed to write history image; skipping: {e}");
                        return RecordOutcome::Skipped;
                    }
                    Err(e) => {
                        tracing::warn!("History image write task was interrupted; skipping: {e}");
                        return RecordOutcome::Skipped;
                    }
                };
                // The user cleared the history during the encode or the write:
                // discard this recording and delete the blob just written
                if generation
                    != self
                        .clear_generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    if freshly_written {
                        let path = self.blob_path(content_hash);
                        let _ = tauri::async_runtime::spawn_blocking(move || {
                            std::fs::remove_file(path)
                        })
                        .await;
                    }
                    tracing::debug!(
                        "History was cleared while recording; discarding this image entry"
                    );
                    return RecordOutcome::Skipped;
                }
                // The footprint must be added **after** the re-check: a
                // discarded recording must not leave the count inflated. When
                // the file already existed (the race guard), the startup total
                // already includes it, so it is not counted twice
                if freshly_written {
                    self.blob_bytes
                        .fetch_add(png_len, std::sync::atomic::Ordering::Relaxed);
                }
                entry.kind = kind::IMAGE.to_string();
                entry.preview = format!("{width}×{height}");
                entry.blob_hash = Some(content_hash.to_string());
            }
            ClipboardContent::Files(paths) => {
                // A non-UTF-8 path fails serde serialization, which takes the
                // whole index down with it; a lossy conversion cannot restore
                // the path reliably either — so skip it (an extremely rare
                // input)
                if paths.iter().any(|p| p.to_str().is_none()) {
                    tracing::info!("File path contains non-UTF-8 bytes; skipping history entry");
                    return RecordOutcome::Skipped;
                }
                entry.kind = kind::FILES.to_string();
                entry.preview = preview_files(&paths);
                entry.files = Some(paths);
            }
            // The type toggle above already stopped this (ImageUnread ⇒
            // enabled = false), so this arm is unreachable; skip as a fallback
            // rather than panicking — failing to record history must never
            // take down the worker
            ClipboardContent::ImageUnread => {
                return RecordOutcome::Skipped;
            }
        }
        // Insert, then evict
        let evicted: Vec<HistoryEntry> = {
            let mut entries = self.lock();
            entries.push(entry);
            let mut evicted = Vec::new();
            while entries.len() > cfg.max_entries.max(1) {
                // The oldest unpinned entry; when everything is pinned there
                // is nothing to evict (pinning is a promise to keep)
                let oldest = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| !e.pinned)
                    .min_by_key(|(_, e)| e.last_copied_at)
                    .map(|(i, _)| i);
                match oldest {
                    Some(idx) => evicted.push(entries.remove(idx)),
                    None => break,
                }
            }
            evicted
        };
        for old in &evicted {
            self.remove_blob_of(old);
        }
        self.save();
        RecordOutcome::Added
    }

    /// Entry list, ordered by [`compare`], with the full text inlined —— every
    /// production path now goes through [`Self::list_meta`] /
    /// [`Self::entry_id_at`]; only test assertions still need the full view
    #[cfg(test)]
    pub fn list(&self, sort: &str) -> Vec<HistoryEntry> {
        let mut list = self.lock().clone();
        list.sort_by(|a, b| compare(sort, a, b));
        list
    }

    /// List projection for the panel list command: same ordering, no inlined
    /// full text
    pub fn list_meta(&self, sort: &str) -> Vec<HistoryEntryMeta> {
        let entries = self.lock();
        let mut idx: Vec<usize> = (0..entries.len()).collect();
        idx.sort_by(|&a, &b| compare(sort, &entries[a], &entries[b]));
        idx.into_iter()
            .map(|i| HistoryEntryMeta::of(&entries[i]))
            .collect()
    }

    /// ID of the nth entry in sorted order, for pasting straight from an Alt+N
    /// slot: sorts indices under the lock instead of cloning the whole table
    pub fn entry_id_at(&self, sort: &str, n: usize) -> Option<String> {
        let entries = self.lock();
        if n >= entries.len() {
            return None;
        }
        let mut idx: Vec<usize> = (0..entries.len()).collect();
        idx.sort_by(|&a, &b| compare(sort, &entries[a], &entries[b]));
        idx.get(n).map(|&i| entries[i].id.clone())
    }

    /// Full-text search for the panel search box: matches preview and full
    /// text by lowercase containment, returning the set of matching IDs
    ///
    /// The semantics match the frontend's
    /// `${preview}\n${text}`.toLowerCase().includes(...) (a files entry
    /// matches through the preview, which holds the file names); ordering is
    /// preserved by the frontend filtering in list order. An empty query
    /// returns every ID — the frontend never searches on an empty query, so
    /// this is only a fallback.
    pub fn search(&self, query: &str) -> Vec<String> {
        let needle = query.to_lowercase();
        let entries = self.lock();
        if needle.is_empty() {
            return entries.iter().map(|e| e.id.clone()).collect();
        }
        entries
            .iter()
            .filter(|e| {
                e.preview.to_lowercase().contains(&needle)
                    || e.text
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
            })
            .map(|e| e.id.clone())
            .collect()
    }

    /// Fetch text by ID, on demand for the preview card: returns (text
    /// truncated to max_chars characters, total character count)
    ///
    /// Returns None for a non-text entry or an unknown ID. Truncation happens
    /// on a character boundary and never splits a multi-byte sequence.
    pub fn entry_text(&self, id: &str, max_chars: usize) -> Option<(String, usize)> {
        let entries = self.lock();
        let text = entries.iter().find(|e| e.id == id)?.text.as_deref()?;
        let total = text.chars().count();
        if total <= max_chars {
            return Some((text.to_string(), total));
        }
        let cut = text
            .char_indices()
            .nth(max_chars)
            .map_or(text.len(), |(i, _)| i);
        Some((text[..cut].to_string(), total))
    }

    /// Clone of the entry with the given ID
    pub fn entry(&self, id: &str) -> Option<HistoryEntry> {
        self.lock().iter().find(|e| e.id == id).cloned()
    }

    /// Delete a single entry, along with its blob
    pub fn delete(&self, id: &str) -> bool {
        let removed = {
            let mut entries = self.lock();
            entries
                .iter()
                .position(|e| e.id == id)
                .map(|idx| entries.remove(idx))
        };
        match removed {
            Some(entry) => {
                self.remove_blob_of(&entry);
                self.save();
                true
            }
            None => false,
        }
    }

    /// Clear the entire history, pinned entries and all blobs included
    pub fn clear(&self) {
        // Bump the generation **before** clearing the table: doing it
        // afterwards leaves a narrower window between the two — a record
        // resuming at exactly that moment reads the old generation and pushes
        // its entry back in anyway
        self.clear_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.lock().clear();
        let _ = std::fs::remove_dir_all(self.dir.join(BLOBS_DIR));
        // Remove the whole sync-files root: after a clear no entry references
        // any batch directory (a batch still being received fails its write
        // and is rejected — under extreme timing, better to fail that one sync
        // than to leave an orphan behind)
        let _ = std::fs::remove_dir_all(&self.sync_root);
        self.blob_bytes
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.lock_preview_cache().clear();
        self.save();
    }

    /// Pin or unpin an entry
    pub fn set_pinned(&self, id: &str, pinned: bool) -> bool {
        let mut entries = self.lock();
        match entries.iter_mut().find(|e| e.id == id) {
            Some(entry) => {
                entry.pinned = pinned;
                drop(entries);
                self.save();
                true
            }
            None => false,
        }
    }

    /// Bytes of disk the history occupies (index + blobs)
    ///
    /// The blobs part comes from the runtime counter (initialized on load,
    /// adjusted on write and delete) rather than a directory walk each time —
    /// the frontend triggers this query after every copy.
    pub fn disk_usage(&self) -> u64 {
        let index = std::fs::metadata(self.dir.join(INDEX_FILE))
            .map(|m| m.len())
            .unwrap_or(0);
        // sync-files is walked directly: the number of batches is on the order
        // of the number of history entries (≤ the entry cap) and each batch
        // holds few files, nowhere near the scale that made the full blobs
        // walk a problem. This query runs on the blocking thread of an async
        // command, so it never occupies the main thread
        let sync_files = walk_dir_bytes(&self.sync_root);
        index + sync_files + self.blob_bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Sweep orphaned sync-files batches; called once at startup
    ///
    /// Where the orphans come from: remote files received while
    /// recordFiles=off — they are written to disk and put on the clipboard but
    /// not recorded in the history, so no entry references them; plus the
    /// remains of deleted history entries and of crashes (half-written
    /// .part files). The rule: delete any batch directory no files entry
    /// references, whole.
    pub fn sweep_orphan_sync_files(&self) {
        let root = self.sync_root.clone();
        let Ok(read) = std::fs::read_dir(&root) else {
            return;
        };
        let referenced: std::collections::HashSet<PathBuf> = self
            .lock()
            .iter()
            .filter_map(|e| e.files.as_ref())
            .flatten()
            .filter(|p| p.starts_with(&root))
            .filter_map(|p| p.parent().map(PathBuf::from))
            .collect();
        let mut removed = 0usize;
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir()
                && !referenced.contains(&path)
                && std::fs::remove_dir_all(&path).is_ok()
            {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(removed, "Removed orphaned sync-files batches");
        }
    }

    /// Read an image entry and decode it to RGBA: the restore path taken when
    /// an entry is selected and copied
    pub fn load_image_rgba(&self, blob_hash: &str) -> std::io::Result<(usize, usize, Vec<u8>)> {
        let bytes = std::fs::read(self.blob_path(blob_hash))?;
        decode_png(&bytes)
    }

    /// PNG for display in the preview card, downsampled when the long edge
    /// exceeds [`PREVIEW_IMAGE_MAX_EDGE`]
    ///
    /// The card displays at a limited size, so pushing the full-size PNG (~4MB
    /// for a 5K screenshot) over IPC just to have the WebView decode it into
    /// tens of MB of bitmap is pure waste; downsampling cuts both the bytes
    /// transferred and the memory used to decode by an order of magnitude.
    /// **Applies to the presentation path only**: the blob file and the
    /// restore path ([`Self::load_image_rgba`]) always keep the original
    /// resolution. A failed downsample falls back to the original bytes —
    /// better slow than a broken image.
    pub fn preview_png(&self, blob_hash: &str) -> std::io::Result<Vec<u8>> {
        if let Some(hit) = self.preview_cache_get(blob_hash) {
            return Ok(hit);
        }
        let bytes = std::fs::read(self.blob_path(blob_hash))?;
        // Probing the dimensions only parses the PNG header, it does not
        // decode pixels; a small image passes straight through and takes no
        // cache slot (the frontend already keeps a per-entry Blob LRU, so only
        // downsampled results — the expensive ones to recompute — are cached
        // here)
        let Some((width, height)) = png_dimensions(&bytes) else {
            return Ok(bytes);
        };
        if width.max(height) <= PREVIEW_IMAGE_MAX_EDGE {
            return Ok(bytes);
        }
        match downscale_png(&bytes, width, height) {
            Ok(scaled) => {
                self.preview_cache_put(blob_hash, scaled.clone());
                Ok(scaled)
            }
            Err(e) => {
                tracing::warn!("Failed to downsample preview image; using original: {e}");
                Ok(bytes)
            }
        }
    }

    /// Take the lock on the downsampled preview cache
    fn lock_preview_cache(&self) -> MutexGuard<'_, VecDeque<(String, Vec<u8>)>> {
        self.preview_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Look up the downsampled cache; a hit moves to the back of the queue as
    /// the most recently used
    fn preview_cache_get(&self, blob_hash: &str) -> Option<Vec<u8>> {
        let mut cache = self.lock_preview_cache();
        let idx = cache.iter().position(|(key, _)| key == blob_hash)?;
        let hit = cache.remove(idx)?;
        let png = hit.1.clone();
        cache.push_back(hit);
        Some(png)
    }

    /// Write into the downsampled cache, evicting the least recently used once
    /// over capacity
    fn preview_cache_put(&self, blob_hash: &str, png: Vec<u8>) {
        let mut cache = self.lock_preview_cache();
        if cache.len() >= PREVIEW_CACHE_CAP {
            cache.pop_front();
        }
        cache.push_back((blob_hash.to_string(), png));
    }

    /// Path of a cached application icon: the file name is the BLAKE3 of the
    /// application name, so no arbitrary character ever reaches the path
    fn app_icon_path(&self, app_name: &str) -> PathBuf {
        let key = lanecho_core::clipboard::hash_text(app_name);
        self.dir.join(APP_ICONS_DIR).join(format!("{key}.png"))
    }

    /// Cache the icon of a source application; skipped if it already exists,
    /// and a failure is only logged since the icon is optional
    pub fn save_app_icon(&self, app_name: &str, png: &[u8]) {
        let path = self.app_icon_path(app_name);
        if path.exists() {
            return;
        }
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(self.dir.join(APP_ICONS_DIR))?;
            std::fs::write(&path, png)
        };
        if let Err(e) = write() {
            tracing::debug!("Failed to write application icon cache: {e}");
        }
    }

    /// Read the icon of a source application for the preview card; an
    /// uncached icon returns Err and the frontend hides it
    pub fn app_icon_png(&self, app_name: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.app_icon_path(app_name))
    }

    /// Whether this application's icon is already cached; the event pump uses
    /// it to skip capturing and expanding the same icon again
    pub fn has_app_icon(&self, app_name: &str) -> bool {
        self.app_icon_path(app_name).exists()
    }

    /// Path of a blob file; the engine side guarantees the hash is hex, so
    /// there is no path injection surface
    fn blob_path(&self, blob_hash: &str) -> PathBuf {
        self.dir.join(BLOBS_DIR).join(format!("{blob_hash}.png"))
    }

    /// Write an image blob (addressed by hash, skipped if it already exists);
    /// returns whether a file was actually written, since the footprint
    /// counter only adds up real writes
    ///
    /// An associated function that does not take `self`: the path is computed
    /// up front and the caller dispatches it through `spawn_blocking` — the
    /// PNG of a full-screen capture can exceed 20MB, and running the
    /// synchronous `fs::write` inside the async task would occupy a tokio
    /// worker thread (consistent with the rule that all history IO goes on a
    /// blocking thread)
    fn write_blob(blobs_dir: &Path, path: &Path, png: &[u8]) -> std::io::Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        std::fs::create_dir_all(blobs_dir)?;
        std::fs::write(path, png)?;
        Ok(true)
    }

    /// Delete what an entry left on disk and reclaim its footprint count and
    /// downsampled cache: for an image that is the blob file (content_hash
    /// maps 1:1 onto the blob); for files that arrived through sync it is the
    /// sync-files batch directory (**only under the sync-files prefix** — a
    /// files entry from a local copy references the user's own files and must
    /// never be touched)
    fn remove_blob_of(&self, entry: &HistoryEntry) {
        if let Some(paths) = &entry.files {
            let sync_root = &self.sync_root;
            let batch_dirs: std::collections::HashSet<PathBuf> = paths
                .iter()
                .filter(|p| p.starts_with(sync_root))
                .filter_map(|p| p.parent().map(PathBuf::from))
                .collect();
            for dir in batch_dirs {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        let Some(hash) = &entry.blob_hash else {
            return;
        };
        let path = self.blob_path(hash);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&path).is_ok() {
            // saturating: the counter and the disk can drift apart if
            // something outside deletes a file, and it must not wrap around
            let _ = self.blob_bytes.fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |v| Some(v.saturating_sub(size)),
            );
        }
        self.lock_preview_cache().retain(|(key, _)| key != hash);
    }

    /// Persist asynchronously with an atomic write; a failure only warns and
    /// the in-memory state stays in effect, the same trade-off paired makes
    /// Two constraints matter:
    /// - It must use `tauri::async_runtime::spawn_blocking` (the explicit
    ///   global runtime handle): synchronous Tauri commands run on the main
    ///   event loop thread with no ambient tokio context, where a bare
    ///   `tokio::task::spawn_blocking` panics outright
    /// - io_lock serializes the writes to disk, and the snapshot is taken
    ///   **under the lock** — a queued writer always writes the latest
    ///   in-memory state, so an old snapshot can never overwrite newer data
    fn save(&self) {
        use std::sync::atomic::Ordering;
        // A writer is already queued, so skip: it picks up the latest snapshot
        // under the lock, which includes this change
        if self.save_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let entries = Arc::clone(&self.entries);
        let io_lock = Arc::clone(&self.io_lock);
        let pending = Arc::clone(&self.save_pending);
        let dir = self.dir.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _guard = io_lock.lock().unwrap_or_else(PoisonError::into_inner);
            // Reset before taking the snapshot: a change arriving after the
            // reset queues the next writer, while this snapshot (taken after
            // the reset) already covers every change made before it — no
            // window for a lost update
            pending.store(false, Ordering::Release);
            write_index_snapshot(&entries, &dir);
        });
    }

    /// Persist once synchronously, for the flush on exit and for tests; shares
    /// the serializing lock with the async path
    pub fn save_sync(&self) {
        let _guard = self.io_lock.lock().unwrap_or_else(PoisonError::into_inner);
        write_index_snapshot(&self.entries, &self.dir);
    }
}

/// Take the latest snapshot and write it atomically; the caller must already
/// hold the serializing io lock
///
/// A serialization failure **must never write to disk**: an
/// `unwrap_or_default()` here turns the Err into empty bytes that atomically
/// overwrite index.json — silently wiping the entire history, pinned entries
/// included.
fn write_index_snapshot(entries: &Mutex<Vec<HistoryEntry>>, dir: &Path) {
    let snapshot = entries
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    // Compact encoding rather than pretty: the index is a machine-owned file
    // rewritten in full on every change, so with the text inlined the
    // indentation and newlines are pure waste (expand it with jq when
    // investigating)
    let bytes = match serde_json::to_vec(&snapshot) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(
                "Failed to serialize history index; skipping this disk write \
                 (in-memory state remains active): {e}"
            );
            return;
        }
    };
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, dir.join(INDEX_FILE))?;
        Ok(())
    };
    if let Err(e) = write() {
        tracing::warn!("Failed to write history index (in-memory state remains active): {e}");
    }
}

/// Total bytes under a directory (two levels: batch directory / file, enough
/// to cover the sync-files layout)
fn walk_dir_bytes(root: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut total = 0;
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&path) {
                for file in inner.flatten() {
                    total += file.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        } else {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

/// Text preview: the first line truncated to 80 characters (for display only;
/// the storage layer keeps the text byte-for-byte). History entries and sync
/// notifications share the same measure, so the bridge layer must not roll
/// its own.
pub fn preview_text(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default();
    let preview: String = first_line.chars().take(80).collect();
    if preview.len() < text.len() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// File preview: the list of file names truncated to 80 characters
fn preview_files(paths: &[PathBuf]) -> String {
    let names: Vec<&str> = paths
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    let joined = names.join(", ");
    let preview: String = joined.chars().take(80).collect();
    if preview.chars().count() < joined.chars().count() {
        format!("{preview}… ({})", paths.len())
    } else {
        preview
    }
}

/// RGBA → PNG encode
fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        // Compression level Fast rather than the default Balanced: the encode
        // dominates the "copy → shows up in the history list" latency chain
        // (about 90% of it). Measured on a 2560×1440 screenshot:
        //   Balanced  545 ms / 6.89 MB (UI)   704 ms / 6.21 MB (photo)
        //   Fast       24 ms / 6.70 MB (UI)    25 ms / 6.50 MB (photo)
        // 22~28× faster at only ±5% in size (UI content even comes out
        // smaller) —— Fast runs on fdeflate, an implementation tuned for PNG,
        // not a plain drop in compression ratio. Scaled linearly to a 5K
        // external display (14.7MP) the gap is 2.2 s against 0.1 s, which the
        // user feels directly
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

/// Read the pixel dimensions by parsing only the PNG header; cheap, since no
/// pixel data is decoded
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    Some((info.width, info.height))
}

/// Downsample a whole PNG proportionally to a long edge of
/// [`PREVIEW_IMAGE_MAX_EDGE`] and re-encode it
///
/// Lanczos3 convolution (SIMD, fast_image_resize) looks better than the
/// WebView's default scaling of an oversized bitmap; ResizeOptions premultiply
/// and unpremultiply alpha by default, so transparent edges get no black
/// fringe.
fn downscale_png(bytes: &[u8], width: u32, height: u32) -> std::io::Result<Vec<u8>> {
    let (w, h, rgba) = decode_png(bytes)?;
    debug_assert_eq!((w as u32, h as u32), (width, height));
    let scale = f64::from(PREVIEW_IMAGE_MAX_EDGE) / f64::from(width.max(height));
    let dst_w = ((f64::from(width) * scale).round() as u32).max(1);
    let dst_h = ((f64::from(height) * scale).round() as u32).max(1);
    let src = fir::images::Image::from_vec_u8(width, height, rgba, fir::PixelType::U8x4)
        .map_err(std::io::Error::other)?;
    let mut dst = fir::images::Image::new(dst_w, dst_h, fir::PixelType::U8x4);
    fir::Resizer::new()
        .resize(
            &src,
            &mut dst,
            &fir::ResizeOptions::new()
                .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3)),
        )
        .map_err(std::io::Error::other)?;
    encode_png(dst_w as usize, dst_h as usize, dst.buffer()).map_err(std::io::Error::other)
}

/// PNG → RGBA decode, used when restoring to the clipboard
fn decode_png(bytes: &[u8]) -> std::io::Result<(usize, usize, Vec<u8>)> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(std::io::Error::other)?;
    // As of png 0.18 this returns an Option, guarding against overflow in the
    // dimension product; every blob was encoded by this crate and is bounded
    // by MAX_IMAGE_PNG_BYTES, so None can only come from a corrupt file
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| std::io::Error::other("Invalid PNG dimensions"))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(std::io::Error::other)?;
    buf.truncate(info.buffer_size());
    // The encode side always writes RGBA8, so the decode returns the original
    // format
    Ok((info.width as usize, info.height as usize, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lanecho-hist-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cfg(max: usize) -> HistoryConfig {
        HistoryConfig {
            max_entries: max,
            record_text: true,
            record_images: true,
            record_files: true,
        }
    }

    fn text(s: &str) -> (ClipboardContent, String) {
        let c = ClipboardContent::Text(s.to_string());
        let h = c.hash();
        (c, h)
    }

    /// A skipped-read event never enters the history: even with image
    /// recording on (which in theory cannot happen at the same time) it has to
    /// be skipped —— it carries nothing but a sentinel hash, and in the list it
    /// would become an empty entry that can never be displayed
    #[tokio::test]
    async fn image_unread_is_never_recorded() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let content = ClipboardContent::ImageUnread;
        let hash = content.hash();
        assert_eq!(
            store.record(content, &hash, 100, None, None, cfg(10)).await,
            RecordOutcome::Skipped
        );
        assert!(store.list("recent").is_empty());
    }

    /// Dedup: recording the same content again only bumps the count and
    /// refreshes the timestamp, it adds no entry; origin/source_app follow the
    /// most recent copy
    #[tokio::test]
    async fn dedup_bumps_count() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("hello");
        assert_eq!(
            store
                .record(c.clone(), &h, 100, None, Some("Chrome".into()), cfg(10))
                .await,
            RecordOutcome::Added
        );
        assert_eq!(
            store
                .record(c.clone(), &h, 200, Some("peer".into()), None, cfg(10))
                .await,
            RecordOutcome::Bumped
        );
        let list = store.list("recent");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].copy_count, 2);
        assert_eq!(list[0].last_copied_at, 200);
        assert_eq!(list[0].origin.as_deref(), Some("peer"));
        // The most recent write came from a remote, so the local source
        // application is cleared rather than left behind
        assert_eq!(list[0].source_app, None);
    }

    /// A repeat local copy with no source application captured (a restore
    /// wrote it, the capture failed, or we are the frontmost app) keeps the
    /// old value —— restoring from the panel must not wipe out the original
    /// source application
    #[tokio::test]
    async fn bump_keeps_source_app_when_unknown() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("hello");
        store
            .record(c.clone(), &h, 100, None, Some("Chrome".into()), cfg(10))
            .await;
        assert_eq!(
            store.record(c.clone(), &h, 200, None, None, cfg(10)).await,
            RecordOutcome::Bumped
        );
        let list = store.list("recent");
        assert_eq!(list[0].source_app.as_deref(), Some("Chrome"));
        assert_eq!(list[0].origin, None);
        // A newly captured source overwrites as usual
        store
            .record(c.clone(), &h, 300, None, Some("Safari".into()), cfg(10))
            .await;
        assert_eq!(
            store.list("recent")[0].source_app.as_deref(),
            Some("Safari")
        );
    }

    /// Eviction: over the cap the oldest unpinned entry goes; pinned entries
    /// are never evicted
    #[tokio::test]
    async fn eviction_respects_pins() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("a");
        let (b, hb) = text("b");
        let (c, hc) = text("c");
        store.record(a.clone(), &ha, 1, None, None, cfg(2)).await;
        store.record(b.clone(), &hb, 2, None, None, cfg(2)).await;
        // Pin a, the oldest one
        let id_a = store.list("recent").last().unwrap().id.clone();
        assert!(store.set_pinned(&id_a, true));
        // Inserting c triggers eviction: b, the oldest unpinned entry, is
        // removed, and a survives because it is pinned
        store.record(c.clone(), &hc, 3, None, None, cfg(2)).await;
        let hashes: Vec<String> = store
            .list("recent")
            .iter()
            .map(|e| e.content_hash.clone())
            .collect();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&ha));
        assert!(hashes.contains(&hc));
        assert!(!hashes.contains(&hb));
    }

    /// Images: the blob is written to disk, the restore decodes byte-for-byte
    /// identically, and deleting the entry cleans up the blob
    #[tokio::test]
    async fn image_blob_roundtrip_and_cleanup() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let rgba: Vec<u8> = (0..2 * 2 * 4).map(|i| (i * 7) as u8).collect();
        let content = ClipboardContent::Image {
            width: 2,
            height: 2,
            rgba: rgba.clone(),
        };
        let hash = content.hash();
        assert_eq!(
            store
                .record(content.clone(), &hash, 1, None, None, cfg(10))
                .await,
            RecordOutcome::Added
        );

        let entry = &store.list("recent")[0];
        assert_eq!(entry.kind, kind::IMAGE);
        assert_eq!(entry.preview, "2×2");
        let (w, h, back) = store.load_image_rgba(&hash).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(back, rgba, "PNG round trip must restore bytes exactly");

        let id = entry.id.clone();
        assert!(store.delete(&id));
        assert!(
            !store.blob_path(&hash).exists(),
            "Deleting an entry must remove its blob"
        );
    }

    /// Ordering: pinned always comes first; frequent orders by count
    #[tokio::test]
    async fn sorting_modes() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("a");
        let (b, hb) = text("b");
        store.record(a.clone(), &ha, 1, None, None, cfg(10)).await;
        store.record(b.clone(), &hb, 2, None, None, cfg(10)).await;
        store.record(a.clone(), &ha, 3, None, None, cfg(10)).await; // a: count 2, most recent

        // recent: a (3) comes first
        assert_eq!(store.list("recent")[0].content_hash, ha);
        // frequent: a (count 2) comes first
        assert_eq!(store.list("frequent")[0].content_hash, ha);
        // Once b is pinned it always comes first
        let id_b = store
            .list("recent")
            .iter()
            .find(|e| e.content_hash == hb)
            .unwrap()
            .id
            .clone();
        store.set_pinned(&id_b, true);
        assert_eq!(store.list("recent")[0].content_hash, hb);
        assert_eq!(store.list("frequent")[0].content_hash, hb);
    }

    /// Type toggles: a disabled type is skipped outright
    #[tokio::test]
    async fn type_switch_filters() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("x");
        let off = HistoryConfig {
            record_text: false,
            ..cfg(10)
        };
        assert_eq!(
            store.record(c.clone(), &h, 1, None, None, off).await,
            RecordOutcome::Skipped
        );
        assert!(store.list("recent").is_empty());
    }

    /// Persistence round trip: after a save, reloading returns the entry intact
    #[tokio::test]
    async fn persistence_roundtrip() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("keep");
        store.record(c.clone(), &h, 1, None, None, cfg(10)).await;
        store.save_sync();
        let reloaded = HistoryStore::load(&dir.0);
        assert_eq!(reloaded.list("recent").len(), 1);
        assert_eq!(reloaded.list("recent")[0].text.as_deref(), Some("keep"));
    }

    /// List projection: carries no full text, and text_len and the ordering
    /// match the full list
    #[tokio::test]
    async fn list_meta_carries_no_fulltext() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("First entry");
        let (b, hb) = text("second");
        store.record(a, &ha, 1, None, None, cfg(10)).await;
        store.record(b, &hb, 2, None, None, cfg(10)).await;
        let metas = store.list_meta("recent");
        let full = store.list("recent");
        assert_eq!(metas.len(), 2);
        for (meta, entry) in metas.iter().zip(full.iter()) {
            assert_eq!(
                meta.id, entry.id,
                "Projection order must match the full list"
            );
            assert_eq!(meta.text_len, entry.text.as_deref().map_or(0, str::len));
        }
        // No text field may appear in the serialized payload — that is the
        // whole point of the lighter projection
        let json = serde_json::to_string(&metas).unwrap();
        assert!(!json.contains("\"text\":"));
        assert!(json.contains("\"textLen\":"));
    }

    /// Slot lookup: matches the list ordering, and out of range returns None
    #[tokio::test]
    async fn entry_id_at_matches_list_order() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("a");
        let (b, hb) = text("b");
        store.record(a, &ha, 1, None, None, cfg(10)).await;
        store.record(b, &hb, 2, None, None, cfg(10)).await;
        let list = store.list("recent");
        assert_eq!(
            store.entry_id_at("recent", 0).as_deref(),
            Some(&*list[0].id)
        );
        assert_eq!(
            store.entry_id_at("recent", 1).as_deref(),
            Some(&*list[1].id)
        );
        assert_eq!(store.entry_id_at("recent", 2), None);
    }

    /// Search: matches on preview and on full text, case-insensitively, and an
    /// empty query returns everything
    #[tokio::test]
    async fn search_matches_preview_and_fulltext() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("Hello World\nHidden content on the second line");
        let (b, hb) = text("Other");
        store.record(a, &ha, 1, None, None, cfg(10)).await;
        store.record(b, &hb, 2, None, None, cfg(10)).await;
        // Case-insensitive hit on the preview
        assert_eq!(store.search("hello").len(), 1);
        // Hit on the second line: the preview only holds the first line, which
        // proves the search covers the full text
        assert_eq!(store.search("hidden").len(), 1);
        assert_eq!(store.search("missing phrase").len(), 0);
        assert_eq!(store.search("").len(), 2);
    }

    /// Text on demand: truncation lands on a character boundary and the total
    /// character count comes back
    #[tokio::test]
    async fn entry_text_truncates_on_char_boundary() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("échoabc");
        store.record(c, &h, 1, None, None, cfg(10)).await;
        let id = store.list("recent")[0].id.clone();
        let (sliced, total) = store.entry_text(&id, 4).unwrap();
        assert_eq!(sliced, "écho");
        assert_eq!(total, 7);
        // Returned verbatim when no truncation is needed
        let (full, total) = store.entry_text(&id, 100).unwrap();
        assert_eq!(full, "échoabc");
        assert_eq!(total, 7);
        assert!(store.entry_text("missing", 100).is_none());
    }

    /// Preview card downsampling: an oversized long edge is scaled
    /// proportionally down to the cap, a small image passes through untouched,
    /// and the cache is hit
    #[tokio::test]
    async fn preview_png_downscales_large_images() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        // A 1200×900 gradient, long edge over 800
        let (w, h) = (1200usize, 900usize);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let content = ClipboardContent::Image {
            width: w,
            height: h,
            rgba,
        };
        let hash = content.hash();
        store.record(content, &hash, 1, None, None, cfg(10)).await;

        let scaled = store.preview_png(&hash).unwrap();
        let (sw, sh) = png_dimensions(&scaled).unwrap();
        assert_eq!(
            (sw, sh),
            (800, 600),
            "Long edge must hit the cap with proportional scaling"
        );
        let original = std::fs::read(store.blob_path(&hash)).unwrap();
        assert!(
            scaled.len() < original.len(),
            "Downsampling must reduce the byte count"
        );
        // The blob itself must keep the original resolution: that is the floor
        // on restore fidelity
        let (bw, bh, _) = store.load_image_rgba(&hash).unwrap();
        assert_eq!((bw, bh), (w, h));
        // The second call hits the cache and returns the same result
        assert_eq!(store.preview_png(&hash).unwrap(), scaled);
        assert_eq!(store.lock_preview_cache().len(), 1);

        // A small image passes through untouched and takes no cache slot
        let small = ClipboardContent::Image {
            width: 2,
            height: 2,
            rgba: vec![7u8; 16],
        };
        let small_hash = small.hash();
        store
            .record(small, &small_hash, 2, None, None, cfg(10))
            .await;
        let passthrough = store.preview_png(&small_hash).unwrap();
        assert_eq!(
            passthrough,
            std::fs::read(store.blob_path(&small_hash)).unwrap()
        );
        assert_eq!(store.lock_preview_cache().len(), 1);
    }

    /// Footprint counter: recording adds, deleting reclaims, clearing zeroes,
    /// and it agrees with what the directory actually holds
    #[tokio::test]
    async fn disk_usage_counter_tracks_blobs() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let image = ClipboardContent::Image {
            width: 4,
            height: 4,
            rgba: (0..64).collect(),
        };
        let hash = image.hash();
        store.record(image, &hash, 1, None, None, cfg(10)).await;
        let blob_len = std::fs::metadata(store.blob_path(&hash)).unwrap().len();
        assert_eq!(
            store.blob_bytes.load(std::sync::atomic::Ordering::Relaxed),
            blob_len
        );
        // Load as if after a restart: the value rebuilt by the walk matches the
        // runtime counter
        store.save_sync();
        let reloaded = HistoryStore::load(&dir.0);
        assert_eq!(
            reloaded
                .blob_bytes
                .load(std::sync::atomic::Ordering::Relaxed),
            blob_len
        );
        // Deleting reclaims
        let id = store.list("recent")[0].id.clone();
        store.delete(&id);
        assert_eq!(
            store.blob_bytes.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        // Clearing zeroes it (add one more entry first, then clear)
        let img2 = ClipboardContent::Image {
            width: 2,
            height: 2,
            rgba: vec![9u8; 16],
        };
        let h2 = img2.hash();
        store.record(img2, &h2, 3, None, None, cfg(10)).await;
        store.clear();
        assert_eq!(
            store.blob_bytes.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(store.disk_usage(), {
            std::fs::metadata(store.dir.join(INDEX_FILE))
                .map(|m| m.len())
                .unwrap_or(0)
        });
    }

    /// Clear: entries and blobs all go
    #[tokio::test]
    async fn clear_removes_blobs() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let content = ClipboardContent::Image {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        };
        let hash = content.hash();
        store
            .record(content.clone(), &hash, 1, None, None, cfg(10))
            .await;
        assert!(store.blob_path(&hash).exists());
        store.clear();
        assert!(store.list("recent").is_empty());
        assert!(!store.blob_path(&hash).exists());
    }

    /// Clearing the history while an image is being encoded —— nothing may
    /// survive the clear
    ///
    /// The image branch of `record` has two yield points (the PNG encode and
    /// writing the blob to disk), and the `clear_history` command runs on
    /// another task, so it can cut in at either. Pushing straight onto
    /// `entries` on resume leaves, after a "clear", an entry whose blob is
    /// already gone (a broken image), and `blob_bytes` gets added back after
    /// the `store(0)`, inflating the footprint until the next restart.
    ///
    /// Asserting "empty after the clear" holds under both orderings (a clear
    /// landing after the record should remove it just the same), so the test
    /// is correct whether or not it hits the race; hitting it is what catches
    /// the defect.
    #[tokio::test]
    async fn clear_during_image_encode_leaves_nothing() {
        let dir = TempDir::new();
        let store = Arc::new(HistoryStore::load(&dir.0));

        // An image big enough that the PNG encode takes tens of milliseconds,
        // giving clear a chance to cut in at that yield point
        let (width, height) = (2400usize, 1600usize);
        let content = ClipboardContent::Image {
            width,
            height,
            rgba: vec![0x7fu8; width * height * 4],
        };
        let hash = content.hash();

        let recorder = {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                store
                    .record(content, &hash, 100, None, None, cfg(100))
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        store.clear();
        let _ = recorder.await;

        assert!(
            store.list("recent").is_empty(),
            "Clearing must leave no entries behind"
        );
        let blobs = std::fs::read_dir(dir.0.join("history").join(BLOBS_DIR))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(blobs, 0, "Clearing must leave no blobs behind");
        assert_eq!(
            store.blob_bytes.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "Discarded records must not inflate the usage counter"
        );
    }

    /// **Regression guard**: all three sync-files integrations must point at
    /// the engine's real landing root
    ///
    /// The engine (net.rs) lands remote files in
    /// `<data_dir>/sync-files/<batch>/`, while this module's `dir` field is
    /// `<data_dir>/history` —— writing `self.dir.join(SYNC_FILES_DIR)` in all
    /// three places points at a `history/sync-files` that does not exist:
    /// deleting an entry leaves its batch directory behind (a leak), the
    /// startup sweep spins forever against nothing, and the footprint
    /// statistics undercount. Derive `sync_root` from dir again and this test
    /// goes red.
    #[tokio::test]
    async fn sync_files_cleanup_uses_engine_landing_root() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);

        // Simulate a batch of remote files landed by the engine, rooted at
        // <data_dir>/sync-files
        let root = dir.0.join(lanecho_core::sync::SYNC_FILES_DIR);
        let batch = root.join("cafe0001");
        std::fs::create_dir_all(&batch).unwrap();
        let landed = batch.join("a.txt");
        std::fs::write(&landed, b"remote-bytes").unwrap();
        let content = ClipboardContent::Files(vec![landed]);
        let hash = content.hash();
        store
            .record(content, &hash, 1, Some("peer".into()), None, cfg(10))
            .await;

        // The footprint statistics have to include sync-files
        assert!(
            store.disk_usage() >= "remote-bytes".len() as u64,
            "Disk usage must include sync-files batches"
        );

        // Deleting the entry cleans up the batch directory with it
        let id = store.list("recent")[0].id.clone();
        assert!(store.delete(&id));
        assert!(
            !batch.exists(),
            "Deleting a files entry must also remove its sync-files batch directory"
        );

        // The startup sweep removes an orphaned batch no entry references
        let orphan = root.join("beef0002");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("stale.bin"), b"x").unwrap();
        store.sweep_orphan_sync_files();
        assert!(
            !orphan.exists(),
            "Startup cleanup must remove unreferenced batch directories"
        );

        // Clearing the history removes the whole sync-files root with it
        std::fs::create_dir_all(root.join("dead0003")).unwrap();
        store.clear();
        assert!(
            !root.exists(),
            "Clearing history must also remove the sync-files root"
        );
    }
}
