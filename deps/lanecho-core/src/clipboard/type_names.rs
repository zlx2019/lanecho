//! Pasteboard type / clipboard format name snapshot, feeding the ignore
//! rules' type check (the shell compares it against the user's ignore list).
//!
//! - macOS: NSPasteboard type identifiers, the same namespace the concealed
//!   marker lives in (`org.nspasteboard.*`, `com.agilebits.onepassword`, …)
//! - Windows: registered (custom) clipboard format names — the useful ones
//!   for ignoring are exactly the custom markers ("Clipboard Viewer Ignore"
//!   and friends); predefined formats (CF_TEXT …) carry no registered name
//!   and are skipped
//! - Linux: always empty (querying MIME targets needs X11/Wayland bindings,
//!   same v1 stance as sensitive.rs)
//!
//! Like the concealed check, the snapshot and the content read are not
//! atomic; a change landing between the two belongs to the next round.

/// Type identifiers of the current clipboard content
#[cfg(target_os = "macos")]
pub fn read() -> Vec<String> {
    // autoreleasepool: same reasoning as sensitive.rs — the caller is a
    // long-lived tokio worker thread with no implicit pool
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        let Some(types) = pasteboard.types() else {
            return Vec::new();
        };
        types.iter().map(|t| t.to_string()).collect()
    })
}

/// Registered format names of the current clipboard content
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "剪贴板格式枚举为系统调用序列; OpenClipboard 失败时返回空快照, 无指针逃逸"
)]
pub fn read() -> Vec<String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, GetClipboardFormatNameW, OpenClipboard,
    };
    // Enumeration requires holding the clipboard open; when another process
    // holds it, return an empty snapshot — the type rule just cannot judge
    // this round (lenient), and the next change gets a fresh chance
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut format = unsafe { EnumClipboardFormats(0) };
    while format != 0 {
        let mut buffer = [0u16; 256];
        let len =
            unsafe { GetClipboardFormatNameW(format, buffer.as_mut_ptr(), buffer.len() as i32) };
        // len == 0 means a predefined format with no registered name; those
        // are not usable as ignore-list entries and are skipped
        if len > 0 {
            names.push(String::from_utf16_lossy(&buffer[..len as usize]));
        }
        format = unsafe { EnumClipboardFormats(format) };
    }
    unsafe { CloseClipboard() };
    names
}

/// Always empty on this platform
#[cfg(not(any(target_os = "macos", windows)))]
pub fn read() -> Vec<String> {
    Vec::new()
}
