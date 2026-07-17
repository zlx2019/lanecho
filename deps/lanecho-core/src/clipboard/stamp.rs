//! 平台剪贴板变化戳(方案决策 #4)
//!
//! 用单次廉价系统调用判断"剪贴板变没变", 变了才做真正的内容读取,
//! 避免每个轮询周期盲读图像等大内容:
//! - macOS: `NSPasteboard.changeCount`(每次写剪贴板自增)
//! - Windows: `GetClipboardSequenceNumber`(无需打开剪贴板)
//! - Linux: 无统一廉价戳, 返回 None, 上层退化为读文本比对

/// 本平台是否提供廉价变化戳
pub fn supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// 读取当前变化戳; 无戳平台恒返回 None
#[cfg(target_os = "macos")]
pub fn read() -> Option<i64> {
    // objc2 生成的安全绑定: NSPasteboard 可从任意线程访问
    let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
    Some(pasteboard.changeCount() as i64)
}

/// 读取当前变化戳; 无戳平台恒返回 None
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetClipboardSequenceNumber 为纯读取系统调用, 无指针与内存安全影响"
)]
pub fn read() -> Option<i64> {
    let seq = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
    Some(i64::from(seq))
}

/// 读取当前变化戳; 无戳平台恒返回 None
#[cfg(not(any(target_os = "macos", windows)))]
pub fn read() -> Option<i64> {
    None
}
