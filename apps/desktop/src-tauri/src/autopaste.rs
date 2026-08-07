//! Auto-paste on selection: once an entry has been restored to the clipboard,
//! synthesize one paste keystroke into the application that had focus before
//! the panel opened.
//!
//! Off by default, and only ever reached from the two restore paths (picking
//! an entry in the panel, and the numbered slot hotkeys).
//!
//! Two conditions gate it:
//! - **Platform support.** macOS and Windows can synthesize the keystroke.
//!   Linux would need X11/XTEST bindings, so it reports unsupported and the
//!   settings toggle hides itself there.
//! - **Permission.** Without Accessibility permission macOS drops a
//!   synthesized event *silently* — no error, no effect — so switching the
//!   setting on has to ask for it right then, or all the user gets is a
//!   toggle that does nothing. Windows needs no permission.
//!
//! **The key must not go out before the panel has really hidden.** The panel
//! holds keyboard focus while it is up, so an early keystroke lands in its
//! search field instead of the target application; see `paste_after_dismiss`.

use std::time::Duration;

use tauri::Manager;

/// How long to wait between dismissing the panel and sending the key
///
/// Hiding a window and handing focus back are both asynchronous, and no event
/// reports "the previous application has focus again". The native client
/// allows 80ms for an AppKit panel; a webview window is heavier to tear down,
/// so this leaves a little more room.
const FOCUS_SETTLE: Duration = Duration::from_millis(120);

/// Whether this platform can synthesize the paste keystroke at all
pub fn is_supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Whether the operating system currently allows synthesizing it (the macOS
/// Accessibility permission; nothing to check on the other platforms)
pub fn is_permitted() -> bool {
    platform::is_permitted()
}

/// Asks for the permission the keystroke needs; returns whether it is granted
/// as of now
///
/// On macOS this raises the system authorization prompt **and registers the
/// application in the Accessibility list** — without that call the user has to
/// add the binary by hand with the + button. A grant does not reach the
/// running process, so the caller has to say that a restart is needed.
pub fn request_permission() -> bool {
    platform::request_permission()
}

/// Dismisses the panel if it is up, then pastes into whatever has focus
///
/// Returns immediately: the wait for focus to settle runs on a task.
pub fn paste_after_dismiss(app: &tauri::AppHandle) {
    // A missing permission is not reported here — the settings toggle is what
    // asks for it, and failing every paste with a dialog would be worse than
    // doing nothing
    if !is_supported() || !is_permitted() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Dismissing the panel is what hands focus back to the application
        // being pasted into, and doing it here rather than leaving it to the
        // caller keeps the timing in one place. Only when the panel is really
        // up, though: a slot hotkey fires with it closed, and the target
        // application already has focus — hiding then would touch window
        // state for nothing
        let panel_up = app
            .get_webview_window("panel")
            .and_then(|panel| panel.is_visible().ok())
            .unwrap_or(false);
        if panel_up {
            crate::hide_panel_impl(&app);
        }
        tokio::time::sleep(FOCUS_SETTLE).await;
        platform::send_paste();
    });
}

#[cfg(target_os = "macos")]
mod platform {
    #![expect(
        unsafe_code,
        reason = "Accessibility and CGEvent are plain C APIs with no safe binding in the crates already used here"
    )]

    use std::ffi::c_void;

    /// `kVK_ANSI_V`
    const KEY_V: u16 = 0x09;
    /// `kCGEventFlagMaskCommand`
    const FLAG_COMMAND: u64 = 0x0010_0000;
    /// `kCGEventSourceStateCombinedSessionState`: the path a real keyboard
    /// takes
    const SOURCE_COMBINED_SESSION: i32 = 0;
    /// `kCGHIDEventTap`: posted below every application, so the event reaches
    /// whichever one is frontmost
    const TAP_HID: u32 = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        /// Whether this process is trusted for Accessibility
        fn AXIsProcessTrusted() -> u8;
        /// The same check, with an options dictionary that can raise the
        /// system authorization prompt
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceCreate(state_id: i32) -> *mut c_void;
        fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, key_down: u8) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut c_void);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// Whether Accessibility permission is granted
    pub fn is_permitted() -> bool {
        // SAFETY: an argument-free query that only reads process state
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Raises the system authorization prompt and reports the state after it
    pub fn request_permission() -> bool {
        use objc2_foundation::{NSDictionary, NSNumber, NSString};

        // The option key is written out rather than linked:
        // kAXTrustedCheckOptionPrompt is a mutable global, and reading one
        // through FFI buys nothing over the literal it holds
        let key = NSString::from_str("AXTrustedCheckOptionPrompt");
        let value = NSNumber::new_bool(true);
        let options = NSDictionary::from_slices(&[&*key], &[&*value]);
        // SAFETY: NSDictionary is toll-free bridged to CFDictionaryRef and
        // outlives the call, which does not take ownership
        unsafe {
            AXIsProcessTrustedWithOptions(
                std::ptr::from_ref::<NSDictionary<NSString, NSNumber>>(&options).cast(),
            ) != 0
        }
    }

    /// Synthesizes ⌘V
    pub fn send_paste() {
        // SAFETY: every returned pointer is null-checked before use, the
        // modifier is set on an event this call owns, and both the events and
        // the source are released again
        unsafe {
            let source = CGEventSourceCreate(SOURCE_COMBINED_SESSION);
            if source.is_null() {
                return;
            }
            // The modifier goes on both halves: an application reading the
            // key-up alone still has to see ⌘ held
            for key_down in [1u8, 0u8] {
                let event = CGEventCreateKeyboardEvent(source, KEY_V, key_down);
                if event.is_null() {
                    continue;
                }
                CGEventSetFlags(event, FLAG_COMMAND);
                CGEventPost(TAP_HID, event);
                CFRelease(event.cast_const());
            }
            CFRelease(source.cast_const());
        }
    }
}

#[cfg(windows)]
mod platform {
    #![expect(
        unsafe_code,
        reason = "SendInput takes a raw array of INPUT records; the slice below is fully initialized and its length is passed alongside"
    )]

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        VK_CONTROL, VK_V,
    };

    /// Synthesizing input needs no permission on Windows
    pub fn is_permitted() -> bool {
        true
    }

    /// Nothing to ask for, so this only reports that it already works
    pub fn request_permission() -> bool {
        true
    }

    /// Synthesizes Ctrl+V
    pub fn send_paste() {
        /// One key event: the virtual key and whether this is the release
        fn key(vk: VIRTUAL_KEY, release: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if release { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        // Ctrl has to wrap V on both sides, and all four go out in one
        // SendInput call so nothing can interleave between them
        let inputs = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        // SAFETY: the array is fully initialized, its element count and the
        // record size are passed as the API requires
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            );
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    //! Linux: synthesizing a keystroke needs X11/XTEST (or a compositor
    //! specific protocol on Wayland) and no such binding is pulled in, so
    //! auto-paste reports unsupported and the settings toggle hides itself

    /// Never permitted: there is nothing to synthesize the keystroke with
    pub fn is_permitted() -> bool {
        false
    }

    /// Nothing to ask for
    pub fn request_permission() -> bool {
        false
    }

    /// No-op
    pub fn send_paste() {}
}
