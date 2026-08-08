//! Clipboard access and change watching.
//!
//! Responsibilities:
//! - `stamp` (private): the platform change stamp — a cheap "did it change"
//!   answer
//! - this module: reading and classifying content (files > image > text),
//!   writing text, and the watcher task
//! - [`sensitive`][]: concealed marker check (password manager content is
//!   neither broadcast nor recorded in history)
//! - [`frontapp`][]: frontmost application query (the source application on a
//!   history entry)
//!
//! The watcher task only produces [`ClipboardEvent`]; echo suppression and
//! sync decisions live in the sync engine. arboard reads and writes block, so
//! they all go through `spawn_blocking` to stay out of the async context.

pub mod avail;
pub mod frontapp;
pub mod sensitive;
mod stamp;
pub mod type_names;

use std::path::PathBuf;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::{WATCH_INTERVAL, WATCH_INTERVAL_FALLBACK};

/// Clipboard layer errors
#[derive(Debug, Error)]
pub enum ClipboardError {
    /// Accessing the system clipboard failed (busy, content not convertible,
    /// etc.)
    #[error("剪贴板访问失败: {0}")]
    Access(#[from] arboard::Error),
    /// The blocking task was interrupted by the runtime
    #[error("剪贴板任务被中断")]
    TaskJoin,
}

/// Clipboard content (the result of reading and classifying)
///
/// A clipboard often carries several representations at once (copying files
/// also yields the file names as text), so classification takes the most
/// specific one and only that one: files > image > text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    /// Plain text, byte-for-byte as-is (hard rule: never trim, never escape)
    Text(String),
    /// Bitmap (raw RGBA pixels; PNG encoding belongs to the history storage
    /// layer)
    Image {
        /// Width in pixels
        width: usize,
        /// Height in pixels
        height: usize,
        /// RGBA bytes (len = width * height * 4)
        rgba: Vec<u8>,
    },
    /// List of file references (the clipboard itself holds path references,
    /// not file contents)
    Files(Vec<PathBuf>),
    /// An image is present but **its pixels were not read**: the result of
    /// skipping the read when the user turns off image recording
    ///
    /// An event is still emitted rather than silently skipped — a local copy
    /// must advance the engine's LWW baseline (`last_local_copy_ms`), or a
    /// **slightly earlier** text sync from a peer would be judged "newer than
    /// local" and overwrite the image just copied. Consumers treat this as no
    /// content at all.
    ImageUnread,
}

impl ClipboardContent {
    /// BLAKE3 of the content (hex): the single key for echo comparison and
    /// history dedup
    ///
    /// A type prefix goes into the hash so the text "a.txt" and the file list
    /// [a.txt] cannot collide.
    pub fn hash(&self) -> String {
        match self {
            Self::Text(text) => hash_text(text),
            Self::Image {
                width,
                height,
                rgba,
            } => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"i:");
                hasher.update(&width.to_le_bytes());
                hasher.update(&height.to_le_bytes());
                hasher.update(rgba);
                hasher.finalize().to_hex().to_string()
            }
            Self::Files(paths) => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"f:");
                for path in paths {
                    hasher.update(path.as_os_str().as_encoded_bytes());
                    hasher.update(b"\0");
                }
                hasher.finalize().to_hex().to_string()
            }
            // A skipped-read event never enters history (record rejects it at
            // the first type toggle), so this hash only needs to avoid
            // colliding with real content; the watcher clears its dedup state
            // right after sending, so the next entry is not swallowed
            Self::ImageUnread => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"i:unread");
                hasher.finalize().to_hex().to_string()
            }
        }
    }

    /// Short type name (for logging)
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Image { .. } | Self::ImageUnread => "image",
            Self::Files(_) => "files",
        }
    }
}

/// A clipboard change event (produced by the watcher task)
#[derive(Debug, Clone)]
pub struct ClipboardEvent {
    /// The content after the change
    pub content: ClipboardContent,
    /// Content hash (= `content.hash()`, precomputed so consumers do not
    /// rehash a large image)
    pub hash: String,
    /// When the change was detected (Unix milliseconds), the LWW tiebreaker
    pub timestamp_ms: u64,
    /// Pasteboard type / clipboard format snapshot taken at read time (the
    /// ignore-rule type check must judge what was on the clipboard then, not
    /// at whatever later moment the event is consumed). macOS: NSPasteboard
    /// types; Windows: registered format names; Linux: always empty
    pub pasteboard_types: Vec<String>,
    /// Ignore verdict (set by the shell between watcher and engine): skip the
    /// broadcast only — the LWW baseline still advances and LocalCopied still
    /// fires
    pub suppress_broadcast: bool,
    /// Ignore verdict: skip the history recording (rides through the engine
    /// into LocalCopied)
    pub suppress_record: bool,
}

impl ClipboardEvent {
    /// Event with no type snapshot and no ignore verdict (the CLI and the
    /// contentless watcher paths; the shell's ingest fills the verdict in)
    pub fn new(content: ClipboardContent, hash: String, timestamp_ms: u64) -> Self {
        Self {
            content,
            hash,
            timestamp_ms,
            pasteboard_types: Vec::new(),
            suppress_broadcast: false,
            suppress_record: false,
        }
    }
}

/// Hash of text content (same format as the Text branch of
/// [`ClipboardContent::hash`]): used when the sync engine registers an echo,
/// so hashing does not require cloning the whole text
pub fn hash_text(text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"t:");
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Current Unix timestamp in milliseconds
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Read the current clipboard and classify it; an empty clipboard or a failed
/// read returns None (the next round simply retries)
///
/// A fresh `Clipboard` instance per call: negligible cost on mac/Windows, and
/// it sidesteps the Send constraint of holding an instance across threads.
/// Linux takes [`read_text_only`] and never reaches this function.
pub async fn read_content() -> Option<ClipboardContent> {
    run_blocking(read_content_blocking).await
}

/// Text-only read (the Linux fallback path with no change stamp: a blind read
/// every second that touches only the cheap text type)
pub async fn read_text_only() -> Option<ClipboardContent> {
    run_blocking(read_text_only_blocking).await
}

/// The synchronous body of [`read_content`] (runs on a spawn_blocking thread)
fn read_content_blocking() -> Result<Option<ClipboardContent>, arboard::Error> {
    let mut cb = arboard::Clipboard::new()?;
    // The file list is the most specific type, so probe for it first
    if let Ok(paths) = cb.get().file_list()
        && !paths.is_empty()
    {
        return Ok(Some(ClipboardContent::Files(paths)));
    }
    if let Ok(img) = cb.get_image()
        && img.width > 0
        && img.height > 0
    {
        return Ok(Some(ClipboardContent::Image {
            width: img.width,
            height: img.height,
            rgba: img.bytes.into_owned(),
        }));
    }
    match cb.get_text() {
        Ok(text) => Ok(Some(ClipboardContent::Text(text))),
        Err(_) => Ok(None),
    }
}

/// The synchronous body of [`read_text_only`]
fn read_text_only_blocking() -> Result<Option<ClipboardContent>, arboard::Error> {
    let mut cb = arboard::Clipboard::new()?;
    match cb.get_text() {
        Ok(text) => Ok(Some(ClipboardContent::Text(text))),
        Err(_) => Ok(None),
    }
}

/// Read and return the hash alongside the content: BLAKE3 over a whole image
/// has a real cost (around 33MB of RGBA at 4K), so it is computed on the same
/// blocking thread before returning to the async context, never on a tokio
/// worker
async fn read_content_hashed() -> Option<(ClipboardContent, String)> {
    run_blocking(|| Ok(read_content_blocking()?.map(with_hash))).await
}

/// The text-only variant of [`read_content_hashed`] (the Linux blind-read
/// path)
async fn read_text_only_hashed() -> Option<(ClipboardContent, String)> {
    run_blocking(|| Ok(read_text_only_blocking()?.map(with_hash))).await
}

/// Pair content with its hash (used inline inside the blocking closures)
fn with_hash(content: ClipboardContent) -> (ClipboardContent, String) {
    let hash = content.hash();
    (content, hash)
}

/// Write text to the system clipboard (the landing path for a remote sync)
pub async fn write_text(text: String) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut cb = arboard::Clipboard::new()?;
        cb.set_text(text)?;
        Ok(())
    })
    .await
    .map_err(|_| ClipboardError::TaskJoin)?
}

/// Write a bitmap to the system clipboard (the image restore path when a
/// history entry is selected and copied)
pub async fn write_image(width: usize, height: usize, rgba: Vec<u8>) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut cb = arboard::Clipboard::new()?;
        cb.set_image(arboard::ImageData {
            width,
            height,
            bytes: rgba.into(),
        })?;
        Ok(())
    })
    .await
    .map_err(|_| ClipboardError::TaskJoin)?
}

/// Write a list of file references to the system clipboard (the file restore
/// path when a history entry is selected and copied)
pub async fn write_files(paths: Vec<PathBuf>) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut cb = arboard::Clipboard::new()?;
        cb.set().file_list(&paths)?;
        Ok(())
    })
    .await
    .map_err(|_| ClipboardError::TaskJoin)?
}

/// Shared wrapper for blocking reads: failures are logged at debug level and
/// normalized to None
async fn run_blocking<T, F>(f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<Option<T>, arboard::Error> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(content)) => content,
        Ok(Err(e)) => {
            tracing::debug!("剪贴板读取失败(下轮重试): {e}");
            None
        }
        Err(e) => {
            tracing::debug!("剪贴板任务中断: {e}");
            None
        }
    }
}

/// Start the clipboard watcher task: changes are reported over the event
/// channel, and the task exits on its own once the consumer is gone
///
/// Whatever the clipboard already holds at startup becomes the baseline and
/// produces no event (otherwise every launch would rebroadcast the stale
/// content sitting in the clipboard).
///
/// `read_images` is flipped live by the shell layer to follow the image
/// recording setting: when off, an image only gets the cheap presence probe
/// and an [`ClipboardContent::ImageUnread`] event, with no pixels read —
/// images do not take part in cross-device sync anyway, so with history off
/// nobody consumes them.
pub fn spawn_watcher(
    tx: mpsc::Sender<ClipboardEvent>,
    read_images: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reset_dedupe: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let stamped = stamp::supported();
        let period = if stamped {
            WATCH_INTERVAL
        } else {
            WATCH_INTERVAL_FALLBACK
        };
        let mut tick = tokio::time::interval(period);
        // When a read occasionally runs past one period (a large image, or a
        // busy clipboard), do not fire the backlog of missed ticks
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Baseline: the current stamp plus the current content hash
        let mut last_stamp = stamp::read();
        let mut last_hash = baseline_hash(
            stamped,
            read_images.load(std::sync::atomic::Ordering::Relaxed),
        )
        .await;

        loop {
            tick.tick().await;
            // Reset the dedup baseline once recording resumes (a type toggle
            // switched back on, or incognito left): last_hash still remembers
            // content copied while paused, and without clearing it, copying
            // that same content again after resuming would be swallowed by
            // dedup forever — until some other content displaces the
            // baseline, which looks like "recording is on but nothing is
            // recorded"
            if reset_dedupe.swap(false, std::sync::atomic::Ordering::Relaxed) {
                last_hash = None;
            }
            // Fast path: an unchanged stamp means skip, without touching the
            // clipboard content
            if stamped {
                let now = stamp::read();
                if now == last_stamp {
                    continue;
                }
                last_stamp = now;
            }
            // Concealed marker: skip the whole round without reading the
            // content (no broadcast, no history entry)
            if sensitive::is_concealed() {
                tracing::debug!("剪贴板内容带敏感标记, 跳过");
                last_hash = None;
                continue;
            }
            // Skip the read: with image recording off, do not read and decode
            // the whole image (a screenshot can be tens of MB); emit a
            // contentless event just to advance the LWW baseline. The probe
            // must come first — the saving cannot come from "skip the image
            // branch and fall through to text", because a clipboard holding
            // both would then take the text branch and get broadcast,
            // breaking the two-pipeline semantics where the record type
            // toggles never affect sync
            let read = if stamped {
                if !read_images.load(std::sync::atomic::Ordering::Relaxed) && avail::has_image() {
                    tracing::debug!("记录图像已关闭, 跳过图像读取");
                    // Clear the dedup state: this sentinel hash stands for no
                    // real content, and keeping it would swallow the next entry
                    last_hash = None;
                    let content = ClipboardContent::ImageUnread;
                    let hash = content.hash();
                    let event = ClipboardEvent::new(content, hash, now_ms());
                    if tx.send(event).await.is_err() {
                        return;
                    }
                    continue;
                }
                read_content_hashed().await
            } else {
                read_text_only_hashed().await
            };
            let Some((content, hash)) = read else {
                continue;
            };
            // Dedup: the stamp moved but the content did not (e.g. only the
            // format representation changed), and the comparison for the
            // Linux blind read
            if last_hash.as_ref() == Some(&hash) {
                continue;
            }
            last_hash = Some(hash.clone());
            tracing::debug!(kind = content.kind(), "检测到剪贴板变化");
            // Type snapshot for the ignore rules, taken right after the read
            // (a lightweight query, same calling model as the concealed
            // check; the microsecond gap to the read is dwarfed by the
            // polling interval itself)
            let pasteboard_types = type_names::read();
            let event = ClipboardEvent {
                content,
                hash,
                timestamp_ms: now_ms(),
                pasteboard_types,
                suppress_broadcast: false,
                suppress_record: false,
            };
            if tx.send(event).await.is_err() {
                return;
            }
        }
    })
}

/// Content hash for the startup baseline (a failed read means no baseline)
async fn baseline_hash(stamped: bool, read_images: bool) -> Option<String> {
    // Same rule as the main loop: if the existing content carries a concealed
    // marker, do not read it (no baseline, and no dedup state either)
    if sensitive::is_concealed() {
        return None;
    }
    // Likewise: with image recording off, an image already on the clipboard
    // is not worth reading just to compute a baseline
    if stamped && !read_images && avail::has_image() {
        return None;
    }
    let read = if stamped {
        read_content_hashed().await
    } else {
        read_text_only_hashed().await
    };
    read.map(|(_, hash)| hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash must distinguish content types: text and a file list with the
    /// same literal value must not collide
    #[test]
    fn hash_distinguishes_kinds() {
        let text = ClipboardContent::Text("a.txt".into());
        let files = ClipboardContent::Files(vec![PathBuf::from("a.txt")]);
        assert_ne!(text.hash(), files.hash());
    }

    /// Identical content hashes identically, and any difference changes the
    /// hash (including whitespace-only differences)
    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = ClipboardContent::Text("hello".into());
        let b = ClipboardContent::Text("hello".into());
        let c = ClipboardContent::Text("hello ".into());
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), c.hash());
    }

    /// The image hash mixes in the dimensions: the same bytes at a different
    /// width and height must not hash the same
    #[test]
    fn image_hash_includes_dimensions() {
        let rgba = vec![0u8; 16];
        let a = ClipboardContent::Image {
            width: 2,
            height: 2,
            rgba: rgba.clone(),
        };
        let b = ClipboardContent::Image {
            width: 4,
            height: 1,
            rgba,
        };
        assert_ne!(a.hash(), b.hash());
    }

    /// The sentinel hash of a skipped-read event must not collide with any
    /// real content (a collision would let the watcher's dedup swallow a
    /// genuine copy, or let the engine erase it as an echo)
    #[test]
    fn image_unread_hash_is_distinct() {
        let unread = ClipboardContent::ImageUnread.hash();
        let empty_image = ClipboardContent::Image {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        assert_ne!(unread, empty_image.hash());
        assert_ne!(unread, ClipboardContent::Text(String::new()).hash());
        assert_ne!(unread, ClipboardContent::Files(Vec::new()).hash());
    }

    /// The file list hash is order-sensitive (a path list is ordered by
    /// definition)
    #[test]
    fn files_hash_is_order_sensitive() {
        let a = ClipboardContent::Files(vec![PathBuf::from("a"), PathBuf::from("b")]);
        let b = ClipboardContent::Files(vec![PathBuf::from("b"), PathBuf::from("a")]);
        assert_ne!(a.hash(), b.hash());
    }

    /// Round trip through the real clipboard (write → read); it depends on the
    /// system clipboard and overwrites its content, so it is ignored by
    /// default and run manually:
    /// cargo nextest run -p lanecho-core --run-ignored all clipboard_roundtrip
    #[tokio::test]
    #[ignore = "依赖并覆盖系统剪贴板, 仅手动运行"]
    async fn clipboard_roundtrip() {
        let text = format!("lanecho-test-{}", uuid::Uuid::new_v4());
        write_text(text.clone()).await.unwrap();
        let content = read_content().await.unwrap();
        assert_eq!(content, ClipboardContent::Text(text));
    }
}
