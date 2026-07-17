//! 终端输出辅助: 统一的彩色前缀与内容预览格式

/// 成功(绿 ✔)
pub fn ok(msg: &str) {
    println!("\x1b[32m✔\x1b[0m {msg}");
}

/// 信息(蓝 ●)
pub fn info(msg: &str) {
    println!("\x1b[34m●\x1b[0m {msg}");
}

/// 警告(黄 !)
pub fn warn(msg: &str) {
    println!("\x1b[33m!\x1b[0m {msg}");
}

/// 事件行(青色前缀符号 + 内容)
pub fn event(symbol: &str, msg: &str) {
    println!("\x1b[36m{symbol}\x1b[0m {msg}");
}

/// 指纹短码(前 8 位; 指纹不足 8 位时原样返回)
pub fn fp8(fingerprint: &str) -> &str {
    fingerprint.get(..8).unwrap_or(fingerprint)
}

/// 文本预览: 单行化 + 截断(仅供终端展示, 不改动原文)
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
