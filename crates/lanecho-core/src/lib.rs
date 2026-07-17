//! lanecho-core: 局域网剪贴板同步核心引擎
//!
//! 与 UI 完全解耦的纯库, 供 CLI(联调验证)与 Tauri 应用(桌面端)复用。
//! 架构与 deskmate 同源(拷贝改造, 独立演进), 分层设计详见 docs/PLAN.md:
//!
//! ```text
//! ┌─ discovery ─ 节点发现: mDNS 主通道 + UDP 组播兜底(自 deskmate 改造)
//! ├─ identity  ─ 设备身份: UUID + 自签证书 BLAKE3 指纹(自 deskmate 拷贝)
//! ├─ tls       ─ TLS 1.3 双向认证: 指纹 pin, 不走 CA 体系(自 deskmate 拷贝)
//! ├─ protocol  ─ 控制协议: 长度前缀 JSON 帧, echo 自有帧集(格式同源, 帧集新写)
//! └─ sync      ─ 同步引擎: 剪贴板轮询/回声抑制/last-write-wins(全新)
//! ```
//!
pub mod clipboard;
pub mod config;
pub mod discovery;
pub mod identity;
pub mod protocol;
pub mod sync;
pub mod tls;

/// 协议版本号(major.minor), 握手阶段协商, major 不同则拒绝通信。
/// lanecho 协议独立演进, 与 deskmate 协议不兼容(服务名/端口/帧集均不同)。
pub const PROTOCOL_VERSION: &str = "1.0";

/// 默认 TCP 监听端口(控制通道), 可在设置中修改。
/// 与 deskmate(42424)错开, 同机共存不冲突。
pub const DEFAULT_TCP_PORT: u16 = 42524;

/// 默认 UDP 组播发现端口(mDNS 之外的兜底通道), 可在设置中修改
pub const DEFAULT_DISCOVERY_PORT: u16 = 42525;

#[cfg(test)]
mod tests {
    use super::*;

    /// 协议版本必须是 major.minor 两段式, 保证握手协商逻辑可解析
    #[test]
    fn protocol_version_format() {
        let parts: Vec<&str> = PROTOCOL_VERSION.split('.').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
    }

    /// 控制端口与发现端口不能冲突
    #[test]
    fn default_ports_distinct() {
        assert_ne!(DEFAULT_TCP_PORT, DEFAULT_DISCOVERY_PORT);
    }
}
