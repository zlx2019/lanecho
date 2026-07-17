//! 剪贴板历史引擎(方案第 14 节)
//!
//! 存储布局(引擎数据目录下):
//! - `history/index.json` — 全部条目元数据 + 内联文本(原子写: tmp + rename)
//! - `history/blobs/<blake3>.png` — 图像本体, 哈希寻址
//!
//! 语义要点:
//! - 去重: content_hash 命中只涨 copy_count 并刷新 last_copied_at, 不新增条目
//! - 图像与 content_hash 1:1(哈希同源于像素), 条目删除即删 blob
//! - 淘汰: 超限时移除"未固定 + last_copied_at 最旧"; 固定条目不参与淘汰
//! - 文件类型只存路径引用(剪贴板本身即引用, 方案 14.1), 源文件删除后条目失效
//! - 文本逐字节原样内联(仓库铁律), 截断/转义只发生在 preview 展示层

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use lanecho_core::clipboard::ClipboardContent;

/// 索引文件名
const INDEX_FILE: &str = "index.json";
/// 图像本体目录名
const BLOBS_DIR: &str = "blobs";
/// 图像单条上限(按编码后 PNG 字节判, 方案 14.2 默认值)
const MAX_IMAGE_PNG_BYTES: usize = 10 * 1024 * 1024;

/// 内容类型常量(kind 字段)
pub mod kind {
    /// 纯文本
    pub const TEXT: &str = "text";
    /// 图像
    pub const IMAGE: &str = "image";
    /// 文件引用列表
    pub const FILES: &str = "files";
}

/// 一条历史记录(直接作为 DTO: camelCase 序列化)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryEntry {
    /// 稳定 ID(UUID)
    pub id: String,
    /// 内容类型(见 [`kind`])
    pub kind: String,
    /// 文本内容(kind=text 时内联, 逐字节原样)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 图像 blob 哈希(kind=image 时指向 blobs/<hash>.png)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_hash: Option<String>,
    /// 文件路径引用(kind=files)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PathBuf>>,
    /// 列表展示摘要(文本首行截断 / 图像尺寸 / 文件名清单)
    pub preview: String,
    /// 内容哈希(去重键, 与 ClipboardContent::hash 同源)
    pub content_hash: String,
    /// 首次复制时刻(Unix 毫秒)
    pub first_copied_at: u64,
    /// 最近复制时刻(Unix 毫秒)
    pub last_copied_at: u64,
    /// 复制次数(同内容重复复制累加)
    pub copy_count: u32,
    /// 来源: None = 本机复制, Some(设备名) = 远端同步写入
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// 固定置顶(不参与淘汰)
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
            pinned: false,
        }
    }
}

/// 记录时的类型开关与容量快照(取自 Settings)
#[derive(Debug, Clone, Copy)]
pub struct HistoryConfig {
    /// 条目上限
    pub max_entries: usize,
    /// 记录文本
    pub record_text: bool,
    /// 记录图像
    pub record_images: bool,
    /// 记录文件引用
    pub record_files: bool,
}

/// 记录结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// 新增了条目
    Added,
    /// 命中既有条目(仅计数与时间刷新)
    Bumped,
    /// 被类型开关或大小上限跳过
    Skipped,
}

/// 历史存储: 内存表 + 磁盘持久化
pub struct HistoryStore {
    /// 历史目录(<data_dir>/history)
    dir: PathBuf,
    /// 条目表(无固定顺序, 排序在 list 时做; Arc 供落盘任务锁内取最新快照)
    entries: Arc<Mutex<Vec<HistoryEntry>>>,
    /// 落盘串行锁(deskmate history 同款): 并发 save 交错写同一临时文件
    /// 会产生撕裂的 index.json, load 把损坏当空表 —— 静默丢掉全部历史
    io_lock: Arc<Mutex<()>>,
}

impl HistoryStore {
    /// 从数据目录加载(缺失/损坏按空表)
    pub fn load(data_dir: &Path) -> Self {
        let dir = data_dir.join("history");
        let entries = std::fs::read(dir.join(INDEX_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            dir,
            entries: Arc::new(Mutex::new(entries)),
            io_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 取锁(毒锁恢复)
    fn lock(&self) -> MutexGuard<'_, Vec<HistoryEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 记录一次剪贴板内容(事件泵串行调用, 无并发窗口)
    ///
    /// 图像的 PNG 编码在锁外阻塞线程执行; `content_hash` 由调用方传入
    /// (watcher/引擎已算过, 免重复哈希大图)。
    pub async fn record(
        &self,
        content: &ClipboardContent,
        content_hash: &str,
        at: u64,
        origin: Option<String>,
        cfg: HistoryConfig,
    ) -> RecordOutcome {
        // 类型开关
        let enabled = match content {
            ClipboardContent::Text(_) => cfg.record_text,
            ClipboardContent::Image { .. } => cfg.record_images,
            ClipboardContent::Files(_) => cfg.record_files,
        };
        if !enabled {
            return RecordOutcome::Skipped;
        }
        // 去重: 命中只涨计数(origin 以最近一次为准)
        {
            let mut entries = self.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.content_hash == content_hash) {
                entry.copy_count = entry.copy_count.saturating_add(1);
                entry.last_copied_at = at;
                entry.origin = origin;
                drop(entries);
                self.save();
                return RecordOutcome::Bumped;
            }
        }
        // 新条目: 图像先在锁外编码落盘
        let mut entry = HistoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            content_hash: content_hash.to_string(),
            first_copied_at: at,
            last_copied_at: at,
            copy_count: 1,
            origin,
            pinned: false,
            ..Default::default()
        };
        match content {
            ClipboardContent::Text(text) => {
                entry.kind = kind::TEXT.to_string();
                entry.preview = preview_text(text);
                entry.text = Some(text.clone());
            }
            ClipboardContent::Image {
                width,
                height,
                rgba,
            } => {
                // 编码前粗判: 极端大图(>128MB RGBA ≈ 5.7K 见方)直接拒,
                // 免得为注定超限的图白编码数秒(精确上限仍按编码后 PNG 判)
                if rgba.len() > 128 * 1024 * 1024 {
                    tracing::info!(bytes = rgba.len(), "图像原始数据过大, 跳过历史记录");
                    return RecordOutcome::Skipped;
                }
                let (width, height) = (*width, *height);
                let rgba = rgba.clone();
                let png =
                    tauri::async_runtime::spawn_blocking(move || encode_png(width, height, &rgba))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                let Some(png) = png else {
                    tracing::warn!("历史图像 PNG 编码失败, 跳过记录");
                    return RecordOutcome::Skipped;
                };
                if png.len() > MAX_IMAGE_PNG_BYTES {
                    tracing::info!(bytes = png.len(), "历史图像超过单条上限, 跳过记录");
                    return RecordOutcome::Skipped;
                }
                if let Err(e) = self.write_blob(content_hash, &png) {
                    tracing::warn!("历史图像落盘失败, 跳过记录: {e}");
                    return RecordOutcome::Skipped;
                }
                entry.kind = kind::IMAGE.to_string();
                entry.preview = format!("{width}×{height}");
                entry.blob_hash = Some(content_hash.to_string());
            }
            ClipboardContent::Files(paths) => {
                entry.kind = kind::FILES.to_string();
                entry.preview = preview_files(paths);
                entry.files = Some(paths.clone());
            }
        }
        // 插入并淘汰
        let evicted: Vec<HistoryEntry> = {
            let mut entries = self.lock();
            entries.push(entry);
            let mut evicted = Vec::new();
            while entries.len() > cfg.max_entries.max(1) {
                // 未固定中最旧的一条; 全部固定时无法淘汰(固定即承诺保留)
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

    /// 条目列表(pinned 恒顶; sort = "frequent" 按次数, 其余按最近)
    pub fn list(&self, sort: &str) -> Vec<HistoryEntry> {
        let mut list = self.lock().clone();
        if sort == "frequent" {
            list.sort_by(|a, b| {
                b.pinned
                    .cmp(&a.pinned)
                    .then(b.copy_count.cmp(&a.copy_count))
                    .then(b.last_copied_at.cmp(&a.last_copied_at))
            });
        } else {
            list.sort_by(|a, b| {
                b.pinned
                    .cmp(&a.pinned)
                    .then(b.last_copied_at.cmp(&a.last_copied_at))
            });
        }
        list
    }

    /// 按 ID 取条目克隆
    pub fn entry(&self, id: &str) -> Option<HistoryEntry> {
        self.lock().iter().find(|e| e.id == id).cloned()
    }

    /// 删除单条(连带 blob)
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

    /// 清空全部历史(含固定条目与全部 blobs)
    pub fn clear(&self) {
        self.lock().clear();
        let _ = std::fs::remove_dir_all(self.dir.join(BLOBS_DIR));
        self.save();
    }

    /// 固定/取消固定
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

    /// 历史占用磁盘字节数(index + blobs)
    pub fn disk_usage(&self) -> u64 {
        let mut total = std::fs::metadata(self.dir.join(INDEX_FILE))
            .map(|m| m.len())
            .unwrap_or(0);
        if let Ok(blobs) = std::fs::read_dir(self.dir.join(BLOBS_DIR)) {
            for blob in blobs.flatten() {
                total += blob.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        total
    }

    /// 读取图像条目并解码为 RGBA(选中复制的还原路径)
    pub fn load_image_rgba(&self, blob_hash: &str) -> std::io::Result<(usize, usize, Vec<u8>)> {
        let bytes = std::fs::read(self.blob_path(blob_hash))?;
        decode_png(&bytes).map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// blob 文件路径(哈希已由引擎侧保证为 hex, 无路径注入面)
    fn blob_path(&self, blob_hash: &str) -> PathBuf {
        self.dir.join(BLOBS_DIR).join(format!("{blob_hash}.png"))
    }

    /// 写入图像 blob(哈希寻址, 已存在即跳过)
    fn write_blob(&self, blob_hash: &str, png: &[u8]) -> std::io::Result<()> {
        let path = self.blob_path(blob_hash);
        if path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(self.dir.join(BLOBS_DIR))?;
        std::fs::write(path, png)
    }

    /// 删除条目对应的 blob(仅图像类; content_hash 与 blob 1:1, 删条目即删文件)
    fn remove_blob_of(&self, entry: &HistoryEntry) {
        if let Some(hash) = &entry.blob_hash {
            let _ = std::fs::remove_file(self.blob_path(hash));
        }
    }

    /// 异步落盘(原子写; 失败仅告警, 内存态仍生效 —— paired 同款取舍)
    /// 两个关键约束:
    /// - 必须用 `tauri::async_runtime::spawn_blocking`(显式全局运行时句柄):
    ///   同步 Tauri 命令跑在主事件循环线程, 无环境 tokio 上下文,
    ///   裸 `tokio::task::spawn_blocking` 会直接 panic
    /// - io_lock 串行化写盘(deskmate history 同款), 且**锁内**才取快照 ——
    ///   排队的写者总是写当时最新的内存态, 旧快照不会覆盖新数据
    fn save(&self) {
        let entries = Arc::clone(&self.entries);
        let io_lock = Arc::clone(&self.io_lock);
        let dir = self.dir.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _guard = io_lock.lock().unwrap_or_else(PoisonError::into_inner);
            let snapshot = entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let write = || -> std::io::Result<()> {
                std::fs::create_dir_all(&dir)?;
                let tmp = dir.join(format!("{INDEX_FILE}.tmp"));
                std::fs::write(
                    &tmp,
                    serde_json::to_vec_pretty(&snapshot).unwrap_or_default(),
                )?;
                std::fs::rename(&tmp, dir.join(INDEX_FILE))?;
                Ok(())
            };
            if let Err(e) = write() {
                tracing::warn!("历史索引落盘失败(内存态仍生效): {e}");
            }
        });
    }

    /// 同步落盘一次(测试与退出路径用)
    #[cfg(test)]
    fn save_sync(&self) {
        let snapshot = self.lock().clone();
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(
            self.dir.join(INDEX_FILE),
            serde_json::to_vec_pretty(&snapshot).unwrap_or_default(),
        );
    }
}

/// 文本预览: 首行截 80 字符(仅展示; 存储层保持逐字节原样)
fn preview_text(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default();
    let preview: String = first_line.chars().take(80).collect();
    if preview.len() < text.len() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// 文件预览: 文件名清单截 80 字符
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

/// RGBA → PNG 编码
fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

/// PNG → RGBA 解码(还原到剪贴板用)
fn decode_png(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>), png::DecodingError> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    // 编码侧固定 RGBA8, 解码回来即原格式
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

    /// 去重: 同内容重复记录只涨计数刷新时间, 不新增条目
    #[tokio::test]
    async fn dedup_bumps_count() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("hello");
        assert_eq!(
            store.record(&c, &h, 100, None, cfg(10)).await,
            RecordOutcome::Added
        );
        assert_eq!(
            store
                .record(&c, &h, 200, Some("peer".into()), cfg(10))
                .await,
            RecordOutcome::Bumped
        );
        let list = store.list("recent");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].copy_count, 2);
        assert_eq!(list[0].last_copied_at, 200);
        assert_eq!(list[0].origin.as_deref(), Some("peer"));
    }

    /// 淘汰: 超限移除未固定最旧; 固定条目不被淘汰
    #[tokio::test]
    async fn eviction_respects_pins() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("a");
        let (b, hb) = text("b");
        let (c, hc) = text("c");
        store.record(&a, &ha, 1, None, cfg(2)).await;
        store.record(&b, &hb, 2, None, cfg(2)).await;
        // 固定最旧的 a
        let id_a = store.list("recent").last().unwrap().id.clone();
        assert!(store.set_pinned(&id_a, true));
        // 插入 c 触发淘汰: 未固定中最旧的 b 被移除, a(固定)幸存
        store.record(&c, &hc, 3, None, cfg(2)).await;
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

    /// 图像: blob 落盘、还原解码逐字节一致、删除清理 blob
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
            store.record(&content, &hash, 1, None, cfg(10)).await,
            RecordOutcome::Added
        );

        let entry = &store.list("recent")[0];
        assert_eq!(entry.kind, kind::IMAGE);
        assert_eq!(entry.preview, "2×2");
        let (w, h, back) = store.load_image_rgba(&hash).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(back, rgba, "PNG 往返必须逐字节还原");

        let id = entry.id.clone();
        assert!(store.delete(&id));
        assert!(!store.blob_path(&hash).exists(), "删除条目应清理 blob");
    }

    /// 排序: pinned 恒顶; frequent 按次数
    #[tokio::test]
    async fn sorting_modes() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (a, ha) = text("a");
        let (b, hb) = text("b");
        store.record(&a, &ha, 1, None, cfg(10)).await;
        store.record(&b, &hb, 2, None, cfg(10)).await;
        store.record(&a, &ha, 3, None, cfg(10)).await; // a: count 2, 最新

        // recent: a(3) 在前
        assert_eq!(store.list("recent")[0].content_hash, ha);
        // frequent: a(count2) 在前
        assert_eq!(store.list("frequent")[0].content_hash, ha);
        // 固定 b 后恒顶
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

    /// 类型开关: 关闭的类型直接跳过
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
            store.record(&c, &h, 1, None, off).await,
            RecordOutcome::Skipped
        );
        assert!(store.list("recent").is_empty());
    }

    /// 持久化往返: save 后重新加载条目完整
    #[tokio::test]
    async fn persistence_roundtrip() {
        let dir = TempDir::new();
        let store = HistoryStore::load(&dir.0);
        let (c, h) = text("keep");
        store.record(&c, &h, 1, None, cfg(10)).await;
        store.save_sync();
        let reloaded = HistoryStore::load(&dir.0);
        assert_eq!(reloaded.list("recent").len(), 1);
        assert_eq!(reloaded.list("recent")[0].text.as_deref(), Some("keep"));
    }

    /// 清空: 条目与 blobs 全清
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
        store.record(&content, &hash, 1, None, cfg(10)).await;
        assert!(store.blob_path(&hash).exists());
        store.clear();
        assert!(store.list("recent").is_empty());
        assert!(!store.blob_path(&hash).exists());
    }
}
