//! 敏感标记检查: 密码管理器写入的剪贴板内容不广播、不入历史
//! (设计见 docs/PLAN.md 6.3)
//!
//! **当前为 M1 桩实现(恒不敏感), M4 里程碑填实**:
//! - macOS: pasteboard types 含 `org.nspasteboard.ConcealedType`
//! - Windows: 剪贴板 format 含 `ExcludeClipboardContentFromMonitorProcessing`
//! - Linux: `x-kde-passwordManagerHint`(KeePassXC), 其余不处理

/// 当前剪贴板内容是否带敏感标记; 命中时监视层整体跳过(不读内容)
pub fn is_concealed() -> bool {
    false
}
