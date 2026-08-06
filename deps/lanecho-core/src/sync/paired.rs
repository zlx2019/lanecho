//! Persistence for the pairing set (paired.json): the receive side's security
//! boundary data.
//!
//! Pairing is lanecho's first gate: a `ClipboardSync` whose source fingerprint
//! is not in this table is refused. The table is keyed by fingerprint and each
//! side holds its own, written by both during the handshake.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clipboard::now_ms;
use crate::protocol::PeerInfo;

/// Pairing file name
const PAIRED_FILE: &str = "paired.json";

/// One pairing record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedPeer {
    /// Peer certificate fingerprint (the key)
    pub fingerprint: String,
    /// Peer device ID
    pub device_id: String,
    /// Display name at pairing time; for display only, a peer renaming itself
    /// does not affect the pairing
    pub name: String,
    /// Pairing timestamp (Unix milliseconds)
    pub paired_at_ms: u64,
}

/// The pairing set: an in-memory table persisted as JSON
pub(crate) struct PairedStore {
    /// Path on disk (paired.json under the data directory)
    path: PathBuf,
    /// Fingerprint → pairing record
    map: HashMap<String, PairedPeer>,
}

impl PairedStore {
    /// Load from the data directory; a missing file means an empty table, and
    /// a parse failure conservatively means the same — warn, do not crash
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

    /// Whether this fingerprint is paired
    pub(crate) fn contains(&self, fingerprint: &str) -> bool {
        self.map.contains_key(fingerprint)
    }

    /// Path on disk
    pub(crate) fn path(&self) -> PathBuf {
        self.path.clone()
    }

    /// Write one pairing; idempotent, and refreshes the name if it exists
    pub(crate) fn insert(&mut self, info: &PeerInfo) {
        self.map.insert(
            info.fingerprint.clone(),
            PairedPeer {
                fingerprint: info.fingerprint.clone(),
                device_id: info.device_id.clone(),
                name: info.name.clone(),
                paired_at_ms: now_ms(),
            },
        );
    }

    /// Remove one pairing; returns whether it was there
    pub(crate) fn remove(&mut self, fingerprint: &str) -> bool {
        self.map.remove(fingerprint).is_some()
    }

    /// Every pairing record, in a stable order by name
    pub(crate) fn list(&self) -> Vec<PairedPeer> {
        let mut list: Vec<PairedPeer> = self.map.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then(a.fingerprint.cmp(&b.fingerprint)));
        list
    }
}

/// Write one snapshot to disk synchronously, as an atomic write: temporary
/// file plus rename. A failure only warns — the in-memory table still holds,
/// and it is not worth failing the whole pairing operation. Serializing the
/// calls is the caller's job (blocking thread plus the io lock, see
/// `Inner::persist_paired`); this function only performs one complete write.
pub(crate) fn write_snapshot(path: &Path, list: &[PairedPeer]) {
    let write = || -> std::io::Result<()> {
        // A serialization failure is unreachable in theory (every field is a
        // String or u64), but must never overwrite the real file with nothing
        let Ok(bytes) = serde_json::to_vec_pretty(list) else {
            tracing::warn!("配对表序列化失败, 跳过本次落盘");
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        tracing::warn!("配对表落盘失败(内存态仍生效): {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An isolated temporary directory, cleaned up on Drop
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

    /// Insert → persist → reload: the pairing survives intact, and is gone
    /// after removal
    #[tokio::test]
    async fn insert_persist_reload_remove() {
        let dir = TempDir::new();
        let mut store = PairedStore::load(&dir.0);
        assert!(!store.contains("aaa"));

        store.insert(&info("aaa", "A"));
        write_snapshot(&store.path(), &store.list());

        let store2 = PairedStore::load(&dir.0);
        assert!(store2.contains("aaa"));
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].name, "A");

        let mut store3 = store2;
        assert!(store3.remove("aaa"));
        assert!(store3.list().is_empty());
        assert!(!store3.remove("aaa"));
    }

    /// A corrupted paired.json is treated as an empty table, without panicking
    #[test]
    fn corrupted_file_treated_as_empty() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join(PAIRED_FILE), b"{ not json ]").unwrap();
        let store = PairedStore::load(&dir.0);
        assert!(store.list().is_empty());
    }
}
