//! Frontmost application query: history entries record a source application
//! for the preview card to show.
//!
//! The copy happens within one polling period before change detection
//! (≤250ms), so the frontmost application at detection time is the source of
//! the copy — the same approach Maccy takes. Extreme cases (switching apps the
//! instant after a copy) record the wrong one, but the only consequence is a
//! wrong label, which is acceptable.
//!
//! - macOS: localizedName of `NSWorkspace.frontmostApplication`
//! - Windows: `GetForegroundWindow` → process executable name (.exe stripped)
//! - Linux: X11/Wayland offer no uniform cheap interface, so v1 is always None
//!   (the same trade-off as the concealed marker)

/// Display name of the frontmost application; None if the query fails (the
/// entry then carries no source application)
#[cfg(target_os = "macos")]
pub fn frontmost_app_name() -> Option<String> {
    // autoreleasepool: the return value is an autoreleased object and the
    // caller is a long-lived tokio worker thread with no implicit pool, so
    // wrapping keeps objects from accumulating (same convention as
    // sensitive.rs)
    objc2::rc::autoreleasepool(|_| {
        let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        // This process being frontmost (e.g. the instant the panel writes a
        // restored entry) is not a source application: match on pid exactly,
        // not on process name (the dev and packaged builds carry different
        // names, so name comparison is unreliable)
        if app.processIdentifier() == std::process::id() as i32 {
            return None;
        }
        app.localizedName().map(|name| name.to_string())
    })
}

/// Display name of the frontmost application; None if the query fails (the
/// entry then carries no source application)
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetForegroundWindow/QueryFullProcessImageNameW 为只读系统查询, 句柄当场关闭, 无内存安全影响"
)]
pub fn frontmost_app_name() -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        // This process being frontmost (e.g. the instant the panel writes a
        // restored entry) is not a source application (exact pid match)
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        // LIMITED_INFORMATION works on high-integrity processes too (full
        // access rights would be denied)
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        // Process name = executable with the .exe suffix stripped
        // ("chrome.exe" → "chrome")
        std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    }
}

/// Display name of the frontmost application; not implemented on Linux in v1,
/// always None
#[cfg(not(any(target_os = "macos", windows)))]
pub fn frontmost_app_name() -> Option<String> {
    None
}

/// Icon of the frontmost application (PNG bytes, the ~32px size): the image
/// next to the source application on the preview card
///
/// Callers cache by application name and fetch once per application (expanding
/// the TIFF costs milliseconds, so this must stay off hot paths)
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "representationUsingType:properties: 仅因泛型字典参数被 objc2 标记 unsafe, 此处传空字典且键类型正确, 无内存安全影响"
)]
pub fn frontmost_app_icon_png() -> Option<Vec<u8>> {
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};

    objc2::rc::autoreleasepool(|_| {
        let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        if app.processIdentifier() == std::process::id() as i32 {
            return None;
        }
        // icon is a multi-size NSImage (16/32/128/256/512); after expanding
        // the TIFF, pick the bitmap representation closest to 32px — the
        // preview card renders it at 14px, so storing a large one only wastes
        // cache
        let tiff = app.icon()?.TIFFRepresentation()?;
        let reps = NSBitmapImageRep::imageRepsWithData(&tiff);
        let mut best: Option<(isize, objc2::rc::Retained<NSBitmapImageRep>)> = None;
        for rep in reps.iter() {
            let Ok(bitmap) = rep.downcast::<NSBitmapImageRep>() else {
                continue;
            };
            let distance = (bitmap.pixelsWide() - 32).abs();
            if best.as_ref().is_none_or(|(d, _)| distance < *d) {
                best = Some((distance, bitmap));
            }
        }
        let (_, bitmap) = best?;
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &objc2_foundation::NSDictionary::new(),
            )
        }?;
        Some(png.to_vec())
    })
}

/// Icon of the frontmost application; HICON→PNG extraction on Windows is not
/// implemented yet and Linux has no implementation — both are always None, and
/// the preview card falls back to a plain text application name
#[cfg(not(target_os = "macos"))]
pub fn frontmost_app_icon_png() -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    /// Smoke test: the queries must not panic, and a desktop session should
    /// yield a frontmost application name and icon (CI's headless environment
    /// may return None, so this only checks the calls are safe and asserts no
    /// value)
    #[test]
    fn query_does_not_panic() {
        let name = super::frontmost_app_name();
        println!("frontmost_app_name() = {name:?}");
        let icon = super::frontmost_app_icon_png();
        println!(
            "frontmost_app_icon_png() = {:?} bytes",
            icon.map(|b| b.len())
        );
    }
}
