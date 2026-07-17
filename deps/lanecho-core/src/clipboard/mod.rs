//! 剪贴板访问与变化监视(方案决策 #4/#5)
//!
//! 分工:
//! - [`stamp`]: 平台变化戳 —— "变没变"的廉价判断
//! - 本模块: 内容读取分类(files > image > text)、文本写入、监视任务
//! - [`sensitive`]: 敏感标记检查(M1 为桩)
//!
//! 监视任务只负责产出 [`ClipboardEvent`], 回声抑制与同步决策在 sync 引擎;
//! arboard 的读写是阻塞调用, 统一经 `spawn_blocking` 隔离出 async 上下文。

pub mod sensitive;
mod stamp;

use std::path::PathBuf;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::{WATCH_INTERVAL, WATCH_INTERVAL_FALLBACK};

/// 剪贴板层错误
#[derive(Debug, Error)]
pub enum ClipboardError {
    /// 系统剪贴板访问失败(被占用/内容不可转换等)
    #[error("剪贴板访问失败: {0}")]
    Access(#[from] arboard::Error),
    /// blocking 任务被运行时中断
    #[error("剪贴板任务被中断")]
    TaskJoin,
}

/// 剪贴板内容(读取分类结果)
///
/// 剪贴板常同时携带多种表示(复制文件时另有文件名文本), 按
/// "最具体优先"分类: files > image > text, 只取一种。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    /// 纯文本, 逐字节原样(仓库铁律: 不 trim、不转义)
    Text(String),
    /// 位图(RGBA 原始像素; PNG 编码是历史存储层的职责)
    Image {
        /// 像素宽
        width: usize,
        /// 像素高
        height: usize,
        /// RGBA 字节(len = width * height * 4)
        rgba: Vec<u8>,
    },
    /// 文件引用列表(剪贴板本身即路径引用, 不含文件内容)
    Files(Vec<PathBuf>),
}

impl ClipboardContent {
    /// 内容 BLAKE3(hex): 回声比对与历史去重的统一键
    ///
    /// 掺入类型前缀, 避免"文本 a.txt"与"文件列表 [a.txt]"哈希碰撞。
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
        }
    }

    /// 类型短名(日志用)
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Image { .. } => "image",
            Self::Files(_) => "files",
        }
    }
}

/// 剪贴板变化事件(监视任务产出)
#[derive(Debug, Clone)]
pub struct ClipboardEvent {
    /// 变化后的内容
    pub content: ClipboardContent,
    /// 内容哈希(= `content.hash()`, 预计算避免消费方重复算大图)
    pub hash: String,
    /// 检测到变化的时刻(Unix 毫秒), LWW 决胜依据
    pub timestamp_ms: u64,
}

/// 文本内容的哈希(与 [`ClipboardContent::hash`] 的 Text 分支同格式):
/// 同步引擎登记回声时用, 免于为算哈希克隆整段文本
pub fn hash_text(text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"t:");
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// 当前 Unix 毫秒时间戳
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// 读取当前剪贴板并分类; 空剪贴板或读取失败返回 None(下轮自然重试)
///
/// 每次新建 `Clipboard` 实例: mac/Win 开销可忽略, 且规避实例跨线程
/// 持有的 Send 约束; Linux 走 [`read_text_only`], 不经此函数。
pub async fn read_content() -> Option<ClipboardContent> {
    run_blocking(|| {
        let mut cb = arboard::Clipboard::new()?;
        // 文件列表最具体, 最先探测
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
    })
    .await
}

/// 只读文本(Linux 无变化戳的退化路径: 每秒盲读, 只碰廉价的文本类型)
pub async fn read_text_only() -> Option<ClipboardContent> {
    run_blocking(|| {
        let mut cb = arboard::Clipboard::new()?;
        match cb.get_text() {
            Ok(text) => Ok(Some(ClipboardContent::Text(text))),
            Err(_) => Ok(None),
        }
    })
    .await
}

/// 把文本写入系统剪贴板(远端同步落地路径)
pub async fn write_text(text: String) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut cb = arboard::Clipboard::new()?;
        cb.set_text(text)?;
        Ok(())
    })
    .await
    .map_err(|_| ClipboardError::TaskJoin)?
}

/// 把位图写入系统剪贴板(历史条目"选中复制"的图像还原路径)
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

/// 把文件引用列表写入系统剪贴板(历史条目"选中复制"的文件还原路径)
pub async fn write_files(paths: Vec<PathBuf>) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut cb = arboard::Clipboard::new()?;
        cb.set().file_list(&paths)?;
        Ok(())
    })
    .await
    .map_err(|_| ClipboardError::TaskJoin)?
}

/// blocking 读取的公共封装: 失败记 debug 日志并归一为 None
async fn run_blocking<F>(f: F) -> Option<ClipboardContent>
where
    F: FnOnce() -> Result<Option<ClipboardContent>, arboard::Error> + Send + 'static,
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

/// 启动剪贴板监视任务: 变化经事件通道上报, 消费端关闭时任务自行退出
///
/// 启动时的存量内容作为基线, 不产生事件(否则每次启动都会把
/// 剪贴板里躺着的旧内容广播一遍)。
pub fn spawn_watcher(tx: mpsc::Sender<ClipboardEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let stamped = stamp::supported();
        let period = if stamped {
            WATCH_INTERVAL
        } else {
            WATCH_INTERVAL_FALLBACK
        };
        let mut tick = tokio::time::interval(period);
        // 读取偶发超过一个周期(大图/剪贴板被占)时不补发积压的 tick
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // 基线: 当前戳 + 当前内容哈希
        let mut last_stamp = stamp::read();
        let mut last_hash = baseline_hash(stamped).await;

        loop {
            tick.tick().await;
            // 快路径: 变化戳未动直接跳过(不碰剪贴板内容)
            if stamped {
                let now = stamp::read();
                if now == last_stamp {
                    continue;
                }
                last_stamp = now;
            }
            // 敏感标记: 整体跳过, 内容不读取(不广播、不入历史)
            if sensitive::is_concealed() {
                tracing::debug!("剪贴板内容带敏感标记, 跳过");
                last_hash = None;
                continue;
            }
            let content = if stamped {
                read_content().await
            } else {
                read_text_only().await
            };
            let Some(content) = content else { continue };
            let hash = content.hash();
            // 戳变但内容未变(如仅格式表示变化)/ Linux 盲读比对: 去重
            if last_hash.as_ref() == Some(&hash) {
                continue;
            }
            last_hash = Some(hash.clone());
            tracing::debug!(kind = content.kind(), "检测到剪贴板变化");
            let event = ClipboardEvent {
                content,
                hash,
                timestamp_ms: now_ms(),
            };
            if tx.send(event).await.is_err() {
                return;
            }
        }
    })
}

/// 启动基线的内容哈希(读取失败视为无基线)
async fn baseline_hash(stamped: bool) -> Option<String> {
    let content = if stamped {
        read_content().await
    } else {
        read_text_only().await
    };
    content.map(|c| c.hash())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 哈希必须区分内容类型: 同字面值的文本与文件列表不得碰撞
    #[test]
    fn hash_distinguishes_kinds() {
        let text = ClipboardContent::Text("a.txt".into());
        let files = ClipboardContent::Files(vec![PathBuf::from("a.txt")]);
        assert_ne!(text.hash(), files.hash());
    }

    /// 同内容同哈希, 内容有别哈希必变(含仅空白差异)
    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = ClipboardContent::Text("hello".into());
        let b = ClipboardContent::Text("hello".into());
        let c = ClipboardContent::Text("hello ".into());
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), c.hash());
    }

    /// 图像哈希掺入尺寸: 同字节不同宽高不得同哈希
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

    /// 文件列表哈希对顺序敏感(路径列表语义即有序)
    #[test]
    fn files_hash_is_order_sensitive() {
        let a = ClipboardContent::Files(vec![PathBuf::from("a"), PathBuf::from("b")]);
        let b = ClipboardContent::Files(vec![PathBuf::from("b"), PathBuf::from("a")]);
        assert_ne!(a.hash(), b.hash());
    }

    /// 真实剪贴板往返(写→读); 依赖系统剪贴板且会覆盖其内容,
    /// 默认忽略, 本机手动验证: cargo nextest run -p lanecho-core --run-ignored all clipboard_roundtrip
    #[tokio::test]
    #[ignore = "依赖并覆盖系统剪贴板, 仅手动运行"]
    async fn clipboard_roundtrip() {
        let text = format!("lanecho-test-{}", uuid::Uuid::new_v4());
        write_text(text.clone()).await.unwrap();
        let content = read_content().await.unwrap();
        assert_eq!(content, ClipboardContent::Text(text));
    }
}
