//! 配对集合持久化(paired.json): 接收侧的安全边界数据
//!
//! 配对是 lanecho 的第一道门(方案 6.2): `ClipboardSync` 来源指纹
//! 不在本表即拒绝。表按指纹为键, 双方各自持有(方案 B 握手时双写)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clipboard::now_ms;
use crate::protocol::PeerInfo;

/// 配对文件名
const PAIRED_FILE: &str = "paired.json";

/// 一条配对记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedPeer {
    /// 对端证书指纹(键)
    pub fingerprint: String,
    /// 对端设备 ID
    pub device_id: String,
    /// 配对时的展示名(仅展示, 对端改名不影响配对关系)
    pub name: String,
    /// 配对时刻(Unix 毫秒)
    pub paired_at_ms: u64,
}

/// 配对集合: 内存表 + json 落盘
pub(crate) struct PairedStore {
    /// 落盘路径(数据目录下 paired.json)
    path: PathBuf,
    /// 指纹 → 配对记录
    map: HashMap<String, PairedPeer>,
}

impl PairedStore {
    /// 从数据目录加载; 文件缺失视为空表, 解析失败保守视为空表(告警不崩溃)
    pub(crate) fn load(dir: &Path) -> Self {
        let path = dir.join(PAIRED_FILE);
        let map = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<PairedPeer>>(&bytes) {
                Ok(list) => list
                    .into_iter()
                    .map(|p| (p.fingerprint.clone(), p))
                    .collect(),
                Err(e) => {
                    tracing::warn!("paired.json 解析失败, 按空配对表处理: {e}");
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };
        Self { path, map }
    }

    /// 指纹是否已配对
    pub(crate) fn contains(&self, fingerprint: &str) -> bool {
        self.map.contains_key(fingerprint)
    }

    /// 写入一条配对(幂等; 已存在时刷新名称)并返回落盘快照
    pub(crate) fn insert(&mut self, info: &PeerInfo) -> (PathBuf, Vec<PairedPeer>) {
        self.map.insert(
            info.fingerprint.clone(),
            PairedPeer {
                fingerprint: info.fingerprint.clone(),
                device_id: info.device_id.clone(),
                name: info.name.clone(),
                paired_at_ms: now_ms(),
            },
        );
        (self.path.clone(), self.list())
    }

    /// 移除一条配对; 返回是否存在过与落盘快照
    pub(crate) fn remove(&mut self, fingerprint: &str) -> (bool, PathBuf, Vec<PairedPeer>) {
        let existed = self.map.remove(fingerprint).is_some();
        (existed, self.path.clone(), self.list())
    }

    /// 全部配对记录(名称序稳定输出)
    pub(crate) fn list(&self) -> Vec<PairedPeer> {
        let mut list: Vec<PairedPeer> = self.map.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then(a.fingerprint.cmp(&b.fingerprint)));
        list
    }
}

/// 异步落盘(原子写: 临时文件 + rename), 失败仅告警 —— 内存表仍然生效,
/// 代价是进程重启后丢这次变更, 不值得让配对操作整体失败
pub(crate) fn persist(path: PathBuf, list: Vec<PairedPeer>) {
    tokio::task::spawn_blocking(move || {
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&list).unwrap_or_default())?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::warn!("配对表落盘失败(内存态仍生效): {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立临时目录, Drop 时自动清理
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lanecho-paired-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn info(fp: &str, name: &str) -> PeerInfo {
        PeerInfo {
            device_id: format!("dev-{fp}"),
            name: name.into(),
            fingerprint: fp.into(),
            platform: "macos".into(),
            os_version: None,
        }
    }

    /// 插入→落盘→重载, 配对关系应完整存续; 移除后不再包含
    #[tokio::test]
    async fn insert_persist_reload_remove() {
        let dir = TempDir::new();
        let mut store = PairedStore::load(&dir.0);
        assert!(!store.contains("aaa"));

        let (path, list) = store.insert(&info("aaa", "A"));
        // 同步等待落盘完成(测试内直接写, 不走 fire-and-forget)
        std::fs::write(&path, serde_json::to_vec_pretty(&list).unwrap()).unwrap();

        let store2 = PairedStore::load(&dir.0);
        assert!(store2.contains("aaa"));
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].name, "A");

        let mut store3 = store2;
        let (existed, _, list) = store3.remove("aaa");
        assert!(existed);
        assert!(list.is_empty());
        let (existed, _, _) = store3.remove("aaa");
        assert!(!existed);
    }

    /// 损坏的 paired.json 按空表处理, 不 panic
    #[test]
    fn corrupted_file_treated_as_empty() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join(PAIRED_FILE), b"{ not json ]").unwrap();
        let store = PairedStore::load(&dir.0);
        assert!(store.list().is_empty());
    }
}
