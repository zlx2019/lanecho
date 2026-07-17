//! 敏感标记检查: 密码管理器写入的剪贴板内容不广播、不入历史
//! (设计见 docs/PLAN.md 6.3)
//!
//! 各平台以事实标准的剪贴板标记识别:
//! - macOS: pasteboard types 含 `org.nspasteboard.ConcealedType`(1Password/Keychain 等均标)
//! - Windows: 剪贴板 format 含 `ExcludeClipboardContentFromMonitorProcessing`(云剪贴板约定)
//!   或更早的 `Clipboard Viewer Ignore`(KeePass 生态沿用至今)
//! - Linux: 无统一标准且查询 MIME target 需 X11/Wayland 绑定, v1 保持恒不敏感,
//!   `x-kde-passwordManagerHint`(KeePassXC)支持在 BACKLOG v1.x
//!
//! 检查与后续内容读取非原子(间隔毫秒级), 极端竞态下可能漏检一轮;
//! 轮询式剪贴板工具(Maccy 等)同此模式, 接受。

/// 当前剪贴板内容是否带敏感标记; 命中时监视层整体跳过(不读内容)
#[cfg(target_os = "macos")]
pub fn is_concealed() -> bool {
    // autoreleasepool: types() 返回 autoreleased 对象, 而调用方是常驻的
    // tokio worker 线程(无隐式池), 包裹避免对象在无池线程上累积
    objc2::rc::autoreleasepool(|_| {
        // objc2 生成的安全绑定: NSPasteboard 可从任意线程访问(与 stamp.rs 同)
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        // types 为 None(nullable 异常兜底; 空剪贴板实为 Some(空数组))视为不敏感
        let Some(types) = pasteboard.types() else {
            return false;
        };
        types
            .iter()
            .any(|t| t.to_string() == "org.nspasteboard.ConcealedType")
    })
}

/// 当前剪贴板内容是否带敏感标记; 命中时监视层整体跳过(不读内容)
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "RegisterClipboardFormatW/IsClipboardFormatAvailable 为纯查询系统调用, 无需 OpenClipboard, 无指针与内存安全影响"
)]
pub fn is_concealed() -> bool {
    use windows_sys::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    // 新旧两代事实标准都查: 仅设旧标记的工具(老版本 KeePass 等)同样要豁免
    let markers = [
        windows_sys::core::w!("ExcludeClipboardContentFromMonitorProcessing"),
        windows_sys::core::w!("Clipboard Viewer Ignore"),
    ];
    markers.into_iter().any(|name| {
        // 同名格式重复注册恒返回同一 ID(系统原子表查找, 开销可忽略);
        // 返回 0 表示注册失败(原子表耗尽, 极罕见), 视为无标记
        let id = unsafe { RegisterClipboardFormatW(name) };
        id != 0 && unsafe { IsClipboardFormatAvailable(id) != 0 }
    })
}

/// 当前剪贴板内容是否带敏感标记; 命中时监视层整体跳过(不读内容)
#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_concealed() -> bool {
    false
}
