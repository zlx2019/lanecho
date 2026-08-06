//! Clipboard representation query: asks only "is there an image", never takes
//! the data.
//!
//! See [`super::spawn_watcher`] for the use: once the user turns off image
//! recording, images neither enter history nor take part in sync (v1 only
//! syncs text across devices), so reading and decoding a whole image is pure
//! waste (63ms for a 2560×1440 screenshot, around 250ms at 5K). But **the
//! saving must not come from "if no image is read, treat it as text"** — a
//! clipboard holding both image and text would then take the text branch and
//! get broadcast, breaking the hard rule that the record type toggles govern
//! local history only and never affect sync. Hence the cheap up-front
//! question of whether an image is present.
//!
//! Same shape as [`super::sensitive`]: a pure query syscall that never opens
//! or holds the clipboard.

/// Whether the clipboard currently carries an image representation (does not
/// read the data)
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "objc2-app-kit 的 pasteboard 类型常量与 availableTypeFromArray 均为 extern 声明, 纯查询无数据读取"
)]
pub fn has_image() -> bool {
    // autoreleasepool: the caller is a long-lived tokio worker thread with no
    // implicit pool, same as sensitive.rs
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        // availableTypeFromArray rather than types(): it accounts for the
        // system's automatic conversions, which is exactly the question "can
        // arboard get an image" (arboard's macOS path only knows TIFF)
        let wanted = objc2_foundation::NSArray::from_slice(&[
            unsafe { objc2_app_kit::NSPasteboardTypeTIFF },
            unsafe { objc2_app_kit::NSPasteboardTypePNG },
        ]);
        pasteboard.availableTypeFromArray(&wanted).is_some()
    })
}

/// Whether the clipboard currently carries an image representation (does not
/// read the data)
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "IsClipboardFormatAvailable 为纯查询系统调用, 无需 OpenClipboard, 无指针与内存安全影响"
)]
pub fn has_image() -> bool {
    use windows_sys::Win32::System::DataExchange::IsClipboardFormatAvailable;
    // Only CF_DIBV5 is queried: arboard's Windows image path is exactly
    // is_format_avail(CF_DIBV5), and querying the same format is what makes
    // "an image was detected" strictly equivalent to "arboard can read an
    // image". Also checking CF_DIB would, on a (theoretical) CF_DIB-only
    // clipboard, report an image that arboard cannot read, so the round would
    // neither record anything nor fall through to text — an event lost for
    // nothing. Windows synthesizes between CF_BITMAP/CF_DIB/CF_DIBV5, so one
    // query covers all three.
    //
    // The constant is defined inline instead of pulling in windows-sys'
    // `Win32_System_Ole`: dragging in a whole feature gate for one clipboard
    // format ID whose ABI has been fixed since Win32 goes against how this
    // repository keeps features tight
    const CF_DIBV5: u32 = 17;
    unsafe { IsClipboardFormatAvailable(CF_DIBV5) != 0 }
}

/// Whether the clipboard currently carries an image representation (does not
/// read the data)
///
/// Linux: the v1 watcher path reads text only anyway (no cheap change stamp,
/// see stamp.rs) and never reads images, so this is always false — the
/// skip-the-read switch has nothing to do on this platform
#[cfg(not(any(target_os = "macos", windows)))]
pub fn has_image() -> bool {
    false
}
