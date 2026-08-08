//! Frontmost application query: history entries record a source application
//! for the preview card to show.
//!
//! The copy happens within one polling period before change detection
//! (≤250ms), so the frontmost application at detection time is the source of
//! the copy — the same approach Maccy takes. Extreme cases (switching apps the
//! instant after a copy) record the wrong one, but the only consequence is a
//! wrong label, which is acceptable.
//!
//! - macOS: localizedName of `NSWorkspace.frontmostApplication`; the icon
//!   comes from the same application object (TIFF expansion, ~32px pick)
//! - Windows: `GetForegroundWindow` → process executable name (.exe
//!   stripped); the icon is the executable's first icon resource
//!   (`ExtractIconExW` → GDI pixel readout → PNG)
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

/// Full executable path of the frontmost process (shared by the name and the
/// icon queries); None when there is no foreground window, it belongs to this
/// process, or the query fails
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetForegroundWindow/QueryFullProcessImageNameW 为只读系统查询, 句柄当场关闭, 无内存安全影响"
)]
fn frontmost_exe_path() -> Option<String> {
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
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Display name of the frontmost application; None if the query fails (the
/// entry then carries no source application)
#[cfg(windows)]
pub fn frontmost_app_name() -> Option<String> {
    let path = frontmost_exe_path()?;
    // Process name = executable with the .exe suffix stripped
    // ("chrome.exe" → "chrome")
    std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
}

/// Display name of the frontmost application; not implemented on Linux in v1,
/// always None
#[cfg(not(any(target_os = "macos", windows)))]
pub fn frontmost_app_name() -> Option<String> {
    None
}

/// Stable identifier of the frontmost application, the matching key of the
/// ignore-by-application rule: the bundle identifier on macOS
/// ("com.google.Chrome"; display names shift with the system language, the
/// identifier does not)
#[cfg(target_os = "macos")]
pub fn frontmost_app_id() -> Option<String> {
    objc2::rc::autoreleasepool(|_| {
        let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        // Same self-exemption as the name query: a restore write must not
        // read as coming from this process
        if app.processIdentifier() == std::process::id() as i32 {
            return None;
        }
        app.bundleIdentifier().map(|id| id.to_string())
    })
}

/// Stable identifier of the frontmost application: the lowercased executable
/// file name on Windows ("chrome.exe" — there is no bundle identifier, and
/// the full path varies per install)
#[cfg(windows)]
pub fn frontmost_app_id() -> Option<String> {
    let path = frontmost_exe_path()?;
    std::path::Path::new(&path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase)
}

/// Stable identifier of the frontmost application; not implemented on Linux
/// in v1, always None (the ignore-by-application rule cannot judge here)
#[cfg(not(any(target_os = "macos", windows)))]
pub fn frontmost_app_id() -> Option<String> {
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

/// Icon of the frontmost application (PNG bytes, the system large size —
/// 32px at 100% scale): the image next to the source application on the
/// preview card
///
/// The chain is `ExtractIconExW` (the first icon resource of the executable,
/// which is what Explorer shows) → `GetIconInfo` for the colour bitmap →
/// `GetDIBits` for the raw BGRA pixels → PNG. Store applications whose
/// executables sit under WindowsApps may refuse the resource read; the query
/// then returns None and the preview card falls back to the plain name, the
/// same as before this existed.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "ExtractIconExW/GetIconInfo/GetDIBits 为只读查询; 图标与位图句柄在所有返回路径上都被释放, 像素缓冲由 Rust 侧分配并按尺寸校验"
)]
pub fn frontmost_app_icon_png() -> Option<Vec<u8>> {
    use windows_sys::Win32::UI::Shell::ExtractIconExW;
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;

    let path = frontmost_exe_path()?;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut icon = std::ptr::null_mut();
        let extracted = ExtractIconExW(wide.as_ptr(), 0, &mut icon, std::ptr::null_mut(), 1);
        // u32::MAX means the file could not be read at all
        if extracted == 0 || extracted == u32::MAX || icon.is_null() {
            return None;
        }
        let pixels = icon_rgba(icon);
        DestroyIcon(icon);
        let (width, height, rgba) = pixels?;
        encode_icon_png(width, height, &rgba)
    }
}

/// Reads an icon's pixels as RGBA (the colour bitmap of `GetIconInfo`, read
/// out through `GetDIBits` as a top-down 32bpp DIB)
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GDI 句柄查询与像素拷出; 两个位图句柄无论成败都被 DeleteObject, 屏幕 DC 即取即还"
)]
unsafe fn icon_rgba(
    icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
) -> Option<(u32, u32, Vec<u8>)> {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
        GetDIBits, GetObjectW, ReleaseDC,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    unsafe {
        let mut info: ICONINFO = std::mem::zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        // GetIconInfo hands out two bitmaps the caller owns; both must be
        // released on every path below
        let color = info.hbmColor;
        let mask = info.hbmMask;
        let result = (|| {
            // A null colour bitmap is a monochrome icon (double-height mask);
            // application icons are never that shape, fall back to text
            if color.is_null() {
                return None;
            }
            let mut bmp: BITMAP = std::mem::zeroed();
            if GetObjectW(
                color.cast(),
                std::mem::size_of::<BITMAP>() as i32,
                std::ptr::from_mut(&mut bmp).cast(),
            ) == 0
            {
                return None;
            }
            let (width, height) = (bmp.bmWidth, bmp.bmHeight);
            // Sanity bounds: the large system icon is ~32px, scaled displays
            // reach 64; anything wilder means the query went wrong
            if width <= 0 || height <= 0 || width > 512 || height > 512 {
                return None;
            }
            let mut bitmap_info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative height = top-down rows, the order PNG wants
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };
            let mut bgra = vec![0u8; width as usize * height as usize * 4];
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let scanned = GetDIBits(
                dc,
                color,
                0,
                height as u32,
                bgra.as_mut_ptr().cast(),
                &mut bitmap_info,
                DIB_RGB_COLORS,
            );
            ReleaseDC(std::ptr::null_mut(), dc);
            if scanned == 0 {
                return None;
            }
            Some((width as u32, height as u32, bgra_to_rgba(bgra)))
        })();
        if !color.is_null() {
            DeleteObject(color.cast());
        }
        if !mask.is_null() {
            DeleteObject(mask.cast());
        }
        result
    }
}

/// BGRA (the DIB byte order GetDIBits produces) → RGBA
///
/// Icons whose alpha channel is entirely zero are the old mask-based kind:
/// their transparency lives in the separate mask bitmap, and taking the zeros
/// at face value would make the whole icon invisible. They are treated as
/// fully opaque instead — a square backdrop beats an empty slot, and modern
/// application icons all carry a real alpha channel anyway.
#[cfg(any(windows, test))]
fn bgra_to_rgba(mut pixels: Vec<u8>) -> Vec<u8> {
    let opaque_fallback = pixels.chunks_exact(4).all(|px| px[3] == 0);
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
        if opaque_fallback {
            px[3] = 0xff;
        }
    }
    pixels
}

/// RGBA pixels → PNG bytes (a ~32px icon, so compression tuning is
/// irrelevant)
#[cfg(windows)]
fn encode_icon_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Icon of the frontmost application; Linux has no implementation (same
/// trade-off as the name query), always None — the preview card falls back to
/// a plain text application name
#[cfg(not(any(target_os = "macos", windows)))]
pub fn frontmost_app_icon_png() -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    /// Channel swap: BGRA in, RGBA out, alpha untouched when any pixel
    /// carries one
    #[test]
    fn bgra_swaps_to_rgba_and_keeps_real_alpha() {
        // One blue-ish pixel with alpha 200, one red-ish with alpha 0
        let bgra = vec![250u8, 20, 30, 200, 10, 40, 240, 0];
        let rgba = super::bgra_to_rgba(bgra);
        assert_eq!(rgba, vec![30, 20, 250, 200, 240, 40, 10, 0]);
    }

    /// Mask-based icons (alpha all zero) must come out fully opaque, or the
    /// preview card would render an invisible image
    #[test]
    fn zero_alpha_icon_becomes_opaque() {
        let bgra = vec![1u8, 2, 3, 0, 4, 5, 6, 0];
        let rgba = super::bgra_to_rgba(bgra);
        assert_eq!(rgba, vec![3, 2, 1, 0xff, 6, 5, 4, 0xff]);
    }

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
