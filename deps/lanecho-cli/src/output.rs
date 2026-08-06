//! Terminal output helpers: uniform colored prefixes and content previews.

/// Success (green ✔)
pub fn ok(msg: &str) {
    println!("\x1b[32m✔\x1b[0m {msg}");
}

/// Info (blue ●)
pub fn info(msg: &str) {
    println!("\x1b[34m●\x1b[0m {msg}");
}

/// Warning (yellow !)
pub fn warn(msg: &str) {
    println!("\x1b[33m!\x1b[0m {msg}");
}

/// Event line (cyan prefix symbol + content)
pub fn event(symbol: &str, msg: &str) {
    println!("\x1b[36m{symbol}\x1b[0m {msg}");
}

/// Short fingerprint (first 8 chars; returned unchanged when shorter)
pub fn fp8(fingerprint: &str) -> &str {
    fingerprint.get(..8).unwrap_or(fingerprint)
}

/// Text preview: flattened to one line and truncated (terminal display only,
/// the original text is never modified)
pub fn preview(text: &str) -> String {
    const MAX: usize = 60;
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { '·' } else { c })
        .collect();
    if flat.chars().count() <= MAX {
        flat
    } else {
        let cut: String = flat.chars().take(MAX).collect();
        format!("{cut}… ({} 字节)", text.len())
    }
}
