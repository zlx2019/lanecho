//! 会话层: 控制通道帧协议(帧格式与 deskmate 同源, 帧集为 lanecho 自有)
//!
//! 帧格式: 4 字节大端长度前缀 + JSON body。
//! 连接模式为"拨号-事务-即走"(方案决策 #3): 每次配对/同步新建 TLS 连接,
//! 首帧必须是 `Hello`, 事务完成后以 `Bye` 优雅收尾。
//!
//! 会话事务(发起方视角):
//! ```text
//! Hello → HelloAck → PairRequest   → PairResponse          → Bye   (配对)
//! Hello → HelloAck → ClipboardSync → SyncAck|SyncRejected  → Bye   (同步)
//! Hello → HelloAck → Unpair                                → Bye   (解除)
//! ```

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::PROTOCOL_VERSION;

/// 单帧最大长度(1 MiB), 防御恶意超长帧打爆内存
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// 协议层错误
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// 底层 IO 失败(含对端断连)
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 帧长度超出 [`MAX_FRAME_LEN`]
    #[error("帧长度 {0} 字节超过上限 {MAX_FRAME_LEN}")]
    FrameTooLarge(u32),
    /// 消息 JSON 编解码失败
    #[error("消息编解码失败: {0}")]
    Codec(#[from] serde_json::Error),
    /// 协议 major 版本不一致, 拒绝通信
    #[error("协议版本不兼容: 对端 {peer}, 本机 {local}")]
    VersionMismatch {
        /// 对端版本
        peer: String,
        /// 本机版本
        local: String,
    },
    /// 收到不符合当前会话状态的消息
    #[error("意外的消息: 期望 {expected}, 收到 {got}")]
    Unexpected {
        /// 期望的消息类型
        expected: &'static str,
        /// 实际收到的消息描述
        got: String,
    },
}

/// 设备信息(握手阶段交换; 无头像 —— lanecho 砍掉头像机制)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    /// 设备唯一 ID(UUID)
    pub device_id: String,
    /// 展示名
    pub name: String,
    /// 证书 BLAKE3 指纹(hex)
    pub fingerprint: String,
    /// 平台标识(macos/windows/linux)
    pub platform: String,
    /// 操作系统版本描述(如 "macOS 15.3.1"); 可选, 旧版本可缺省
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
}

/// 同步内容类型
///
/// 刻意用 String 而非枚举: v2 扩展图像等新类型时, 旧版本仍能解析整帧
/// 并以 `SyncRejected(unsupported_type)` 明确拒绝, 而不是 JSON 解析失败断连。
pub mod content_type {
    /// 纯文本(v1 唯一支持的同步类型)
    pub const TEXT: &str = "text";
}

/// 结构化拒因码(deskmate 1.4 经验): 发送端按本机语言渲染,
/// 未知码渲染为通用失败文案。同样用 String 保持演进开放。
pub mod reason_code {
    /// 来源未配对
    pub const NOT_PAIRED: &str = "not_paired";
    /// 载荷超过接收方限制
    pub const TOO_LARGE: &str = "too_large";
    /// 接收方暂停了同步
    pub const DISABLED: &str = "disabled";
    /// 不支持的内容类型(为 v2 预留)
    pub const UNSUPPORTED_TYPE: &str = "unsupported_type";
}

/// 控制通道消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// 会话握手(发起方 → 接收方)
    Hello {
        /// 协议版本(major.minor)
        version: String,
        /// 发起方设备信息
        info: PeerInfo,
    },
    /// 握手应答(接收方设备信息)
    HelloAck {
        /// 协议版本(major.minor)
        version: String,
        /// 接收方设备信息
        info: PeerInfo,
    },
    /// 配对请求(方案 B 握手): 身份已在 Hello 交换, 本帧只表达意图
    PairRequest,
    /// 配对应答: 对端用户在弹窗中的决定
    PairResponse {
        /// 是否接受配对
        accepted: bool,
    },
    /// 解除配对通知(尽力而为; 收不到也不影响安全, 校验在接收侧)
    Unpair,
    /// 剪贴板同步载荷
    ClipboardSync {
        /// 发送方设备内单调递增序号(日志与排查用)
        seq: u64,
        /// 复制发生时刻(Unix 毫秒), LWW 决胜依据(方案决策 #7)
        timestamp_ms: u64,
        /// 内容类型(见 [`content_type`]; v1 仅 text)
        content_type: String,
        /// 内容本体: 逐字节一致送达, 不 trim、不转义
        data: String,
    },
    /// 同步已接受并写入剪贴板
    SyncAck,
    /// 同步被拒
    SyncRejected {
        /// 结构化拒因码(见 [`reason_code`]), 发送端按本机语言渲染
        reason_code: String,
    },
    /// 优雅关闭会话
    Bye,
}

impl ControlMessage {
    /// 消息类型短名(日志与错误信息用)
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::HelloAck { .. } => "hello_ack",
            Self::PairRequest => "pair_request",
            Self::PairResponse { .. } => "pair_response",
            Self::Unpair => "unpair",
            Self::ClipboardSync { .. } => "clipboard_sync",
            Self::SyncAck => "sync_ack",
            Self::SyncRejected { .. } => "sync_rejected",
            Self::Bye => "bye",
        }
    }
}

/// 写一帧: 4 字节大端长度 + JSON body, 随后 flush
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    msg: &ControlMessage,
) -> Result<(), ProtocolError> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge(u32::MAX))?;
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(len));
    }
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// 读一帧并解码; 超长帧直接报错断开, 不读取其内容
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<ControlMessage, ProtocolError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

/// 校验对端协议版本: major 相同即视为兼容
pub fn check_version(peer_version: &str) -> Result<(), ProtocolError> {
    if major_of(peer_version) == major_of(PROTOCOL_VERSION) {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch {
            peer: peer_version.to_string(),
            local: PROTOCOL_VERSION.to_string(),
        })
    }
}

/// 取版本号的 major 段
fn major_of(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试用设备信息
    fn peer_info() -> PeerInfo {
        PeerInfo {
            device_id: "d1".into(),
            name: "n1".into(),
            fingerprint: "f".repeat(64),
            platform: "macos".into(),
            os_version: Some("macOS 15.3".into()),
        }
    }

    /// 帧编解码往返: 各类消息经 duplex 管道后应原样还原
    #[tokio::test]
    async fn frame_roundtrip() {
        let samples = vec![
            ControlMessage::Hello {
                version: PROTOCOL_VERSION.to_string(),
                info: peer_info(),
            },
            ControlMessage::PairRequest,
            ControlMessage::PairResponse { accepted: true },
            // 文本必须逐字节一致: 刻意包含首尾空白与控制字符
            ControlMessage::ClipboardSync {
                seq: 7,
                timestamp_ms: 1_752_000_000_000,
                content_type: content_type::TEXT.to_string(),
                data: "  你好\n\t emoji🚀 \0 尾巴  ".into(),
            },
            ControlMessage::SyncRejected {
                reason_code: reason_code::NOT_PAIRED.to_string(),
            },
            ControlMessage::Bye,
        ];
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        for msg in &samples {
            write_frame(&mut a, msg).await.unwrap();
            let got = read_frame(&mut b).await.unwrap();
            assert_eq!(
                serde_json::to_string(&got).unwrap(),
                serde_json::to_string(msg).unwrap()
            );
        }
    }

    /// 同步消息序列化后内容字段不得被改动(逐字节一致性的护栏)
    #[test]
    fn sync_data_is_byte_exact() {
        let raw = "  space  \u{7f} 中文 ";
        let msg = ControlMessage::ClipboardSync {
            seq: 0,
            timestamp_ms: 0,
            content_type: content_type::TEXT.to_string(),
            data: raw.into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        match serde_json::from_str(&json).unwrap() {
            ControlMessage::ClipboardSync { data, .. } => assert_eq!(data, raw),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// 未知内容类型必须能解析为普通字符串(v2 演进护栏:
    /// 旧版本收到新类型要走 SyncRejected 路径而不是解析失败断连)
    #[test]
    fn unknown_content_type_still_parses() {
        let json = r#"{"type":"clipboard_sync","seq":1,"timestamp_ms":2,"content_type":"image/png","data":"x"}"#;
        let msg: ControlMessage = serde_json::from_str(json).unwrap();
        let ControlMessage::ClipboardSync { content_type, .. } = msg else {
            panic!("expected clipboard_sync");
        };
        assert_eq!(content_type, "image/png");
    }

    /// 旧版本(无 os_version 字段)的设备信息必须能解析;
    /// 未设置的可选字段不得序列化(向后兼容护栏)
    #[test]
    fn peer_info_optional_fields_are_backward_compatible() {
        let legacy = r#"{"device_id":"d","name":"n","fingerprint":"f","platform":"macos"}"#;
        let info: PeerInfo = serde_json::from_str(legacy).unwrap();
        assert_eq!(info.os_version, None);
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("os_version"));
    }

    /// 读取侧必须在解析 body 前就拒绝超长帧
    #[tokio::test]
    async fn oversized_frame_rejected() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let bogus_len = (MAX_FRAME_LEN + 1).to_be_bytes();
        tokio::io::AsyncWriteExt::write_all(&mut a, &bogus_len)
            .await
            .unwrap();
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    /// major 相同兼容, 不同则拒绝
    #[test]
    fn version_compat() {
        assert!(check_version("1.0").is_ok());
        assert!(check_version("1.9").is_ok());
        assert!(matches!(
            check_version("2.0"),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }
}
