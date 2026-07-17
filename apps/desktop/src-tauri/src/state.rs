//! 应用全局状态(tauri manage)与锁辅助

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lanecho_core::sync::SyncEngine;

use crate::settings::Settings;

/// 应用全局状态
pub struct AppState {
    /// 同步引擎句柄(配对/同步/开关/关闭; Arc: 事件泵持有一份做回声撤销)
    pub engine: Arc<SyncEngine>,
    /// 用户设置(Arc: 事件泵闭包持有一份, 不经 app.state 以避开注入时序)
    pub settings: Arc<Mutex<Settings>>,
    /// 引擎数据目录(settings.json / identity.json / paired.json 所在)
    pub data_dir: PathBuf,
}

/// 取锁; 毒锁直接恢复内部数据, 避免一处 panic 连锁毒化全局锁
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
