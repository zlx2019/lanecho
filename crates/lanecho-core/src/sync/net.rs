//! sync 网络会话: 拨号事务与接收循环(帧协议之上, 引擎决策之下)
//!
//! 连接模式为"拨号-事务-即走"(方案决策 #3): 每个事务新建 TLS 连接,
//! Hello 门(版本 + 指纹一致性)之后进入事务, 以 Bye + graceful_close 收尾。
//! 引擎决策(配对判定/同步检查链)在 [`super::Inner`] 的方法里, 本模块只编排 IO。

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::PROTOCOL_VERSION;
use crate::config::{
    CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT, MAX_CONCURRENT_CONNECTIONS, PAIR_DECISION_TIMEOUT,
    REPLY_TIMEOUT,
};
use crate::discovery::Peer;
use crate::identity::DeviceIdentity;
use crate::protocol::{self, ControlMessage, PeerInfo, content_type};
use crate::tls;

use super::{Inner, SyncError};

/// 连接收尾: shutdown 后排空到 EOF(deskmate M1 事故教训:
/// 接收缓冲有未读数据时直接 close 会触发 RST 冲掉在途帧)
pub(super) async fn graceful_close<S>(stream: &mut S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = stream.shutdown().await;
    let mut drain = [0u8; 256];
    // 对端不配合时最多等 3 秒, 不挂死收尾流程
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match stream.read(&mut drain).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
}

/// 拨号对端: 逐候选地址 TCP 连接, TLS 严格 pin 对端证书指纹
async fn connect_peer(
    identity: &DeviceIdentity,
    peer: &Peer,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, SyncError> {
    let config = tls::client_config(identity, Some(peer.info.fingerprint.clone()))?;
    let connector = TlsConnector::from(Arc::new(config));
    for addr in &peer.addrs {
        let Ok(Ok(tcp)) =
            tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((*addr, peer.port))).await
        else {
            continue;
        };
        let _ = tcp.set_nodelay(true);
        // ServerName 仅为 API 要求, 校验走指纹 pin(见 tls 模块)
        let Ok(name) = rustls_pki_types::ServerName::try_from("lanecho") else {
            continue;
        };
        match connector.connect(name, tcp).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                tracing::debug!(%addr, "TLS 连接失败, 尝试下一候选地址: {e}");
            }
        }
    }
    Err(SyncError::PeerUnreachable)
}

/// 出站握手: Hello → HelloAck, 校验版本与"声明指纹 = pin 的指纹"
async fn handshake_out<S>(
    stream: &mut S,
    identity: &DeviceIdentity,
    expected_fp: &str,
) -> Result<PeerInfo, SyncError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    protocol::write_frame(
        stream,
        &ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            info: identity.peer_info(),
        },
    )
    .await?;
    let reply = tokio::time::timeout(REPLY_TIMEOUT, protocol::read_frame(stream))
        .await
        .map_err(|_| SyncError::Timeout("hello_ack"))??;
    let ControlMessage::HelloAck { version, info } = reply else {
        return Err(unexpected("hello_ack", &reply));
    };
    protocol::check_version(&version)?;
    // TLS 层已保证证书 = pin 的指纹, 这里再保证"声明与证书一致"(防冒充)
    if info.fingerprint != expected_fp {
        return Err(SyncError::FingerprintMismatch);
    }
    Ok(info)
}

/// 同步事务(拨号侧): 送达一条剪贴板文本并等待对端裁决
pub(super) async fn sync_transaction(
    identity: &DeviceIdentity,
    peer: &Peer,
    seq: u64,
    timestamp_ms: u64,
    data: String,
) -> Result<(), SyncError> {
    let mut stream = connect_peer(identity, peer).await?;
    handshake_out(&mut stream, identity, &peer.info.fingerprint).await?;
    protocol::write_frame(
        &mut stream,
        &ControlMessage::ClipboardSync {
            seq,
            timestamp_ms,
            content_type: content_type::TEXT.to_string(),
            data,
        },
    )
    .await?;
    let reply = tokio::time::timeout(REPLY_TIMEOUT, protocol::read_frame(&mut stream))
        .await
        .map_err(|_| SyncError::Timeout("sync_reply"))??;
    let result = match reply {
        ControlMessage::SyncAck => Ok(()),
        ControlMessage::SyncRejected { reason_code } => Err(SyncError::Rejected(reason_code)),
        other => Err(unexpected("sync_ack", &other)),
    };
    let _ = protocol::write_frame(&mut stream, &ControlMessage::Bye).await;
    graceful_close(&mut stream).await;
    result
}

/// 配对事务(拨号侧): 发起请求并等待对端用户的弹窗决策(人在环, 长超时)
pub(super) async fn pair_transaction(
    identity: &DeviceIdentity,
    peer: &Peer,
) -> Result<PeerInfo, SyncError> {
    let mut stream = connect_peer(identity, peer).await?;
    let remote = handshake_out(&mut stream, identity, &peer.info.fingerprint).await?;
    protocol::write_frame(&mut stream, &ControlMessage::PairRequest).await?;
    let reply = tokio::time::timeout(PAIR_DECISION_TIMEOUT, protocol::read_frame(&mut stream))
        .await
        .map_err(|_| SyncError::Timeout("pair_response"))??;
    let result = match reply {
        ControlMessage::PairResponse { accepted: true } => Ok(remote),
        ControlMessage::PairResponse { accepted: false } => Err(SyncError::PairRejected),
        other => Err(unexpected("pair_response", &other)),
    };
    let _ = protocol::write_frame(&mut stream, &ControlMessage::Bye).await;
    graceful_close(&mut stream).await;
    result
}

/// 解除配对通知(拨号侧, 尽力而为): 失败无碍, 安全边界在接收侧
pub(super) async fn unpair_transaction(
    identity: &DeviceIdentity,
    peer: &Peer,
) -> Result<(), SyncError> {
    let mut stream = connect_peer(identity, peer).await?;
    handshake_out(&mut stream, identity, &peer.info.fingerprint).await?;
    protocol::write_frame(&mut stream, &ControlMessage::Unpair).await?;
    let _ = protocol::write_frame(&mut stream, &ControlMessage::Bye).await;
    graceful_close(&mut stream).await;
    Ok(())
}

/// 接收循环: 接受入站连接, 并发上限之外的直接拒绝(防 slow-loris)
pub(super) async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    let acceptor = TlsAcceptor::from(Arc::clone(&inner.server_tls));
    let limiter = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        let Ok((tcp, addr)) = listener.accept().await else {
            // accept 瞬时错误(EMFILE 等)让出一拍后重试
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = Arc::clone(&limiter).try_acquire_owned() else {
            tracing::warn!(%addr, "并发连接达上限, 拒绝新连接");
            continue;
        };
        let inner = Arc::clone(&inner);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let _permit = permit;
            serve_conn(inner, acceptor, tcp).await;
        });
    }
}

/// 单连接服务: TLS + Hello 门(限时)后进入事务循环
async fn serve_conn(inner: Arc<Inner>, acceptor: TlsAcceptor, tcp: TcpStream) {
    let _ = tcp.set_nodelay(true);
    // 未认证阶段整体限时, 挡住"连上后不说话"的占坑连接
    let gate = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut stream = acceptor.accept(tcp).await?;
        let cert_fp = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())
            .ok_or(SyncError::FingerprintMismatch)?;
        let first = protocol::read_frame(&mut stream).await?;
        let ControlMessage::Hello { version, info } = first else {
            return Err(unexpected("hello", &first));
        };
        protocol::check_version(&version)?;
        // 声明的指纹必须与 TLS 证书一致(防冒充)
        if info.fingerprint != cert_fp {
            return Err(SyncError::FingerprintMismatch);
        }
        protocol::write_frame(
            &mut stream,
            &ControlMessage::HelloAck {
                version: PROTOCOL_VERSION.to_string(),
                info: inner.identity.peer_info(),
            },
        )
        .await?;
        Ok((stream, info))
    })
    .await;
    let (mut stream, remote) = match gate {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            tracing::debug!("入站握手失败: {e}");
            return;
        }
        Err(_) => {
            tracing::debug!("入站握手超时");
            return;
        }
    };

    // 事务循环: 对端通常单事务即 Bye; 读失败(断连)直接收尾
    loop {
        let Ok(msg) = protocol::read_frame(&mut stream).await else {
            break;
        };
        match msg {
            ControlMessage::PairRequest => {
                let accepted = inner.decide_pair(&remote).await;
                let reply = ControlMessage::PairResponse { accepted };
                if protocol::write_frame(&mut stream, &reply).await.is_err() {
                    break;
                }
            }
            ControlMessage::ClipboardSync {
                timestamp_ms,
                content_type,
                data,
                ..
            } => {
                let reply = match inner
                    .accept_sync(&remote, timestamp_ms, &content_type, data)
                    .await
                {
                    Ok(()) => ControlMessage::SyncAck,
                    Err(code) => ControlMessage::SyncRejected {
                        reason_code: code.to_string(),
                    },
                };
                if protocol::write_frame(&mut stream, &reply).await.is_err() {
                    break;
                }
            }
            ControlMessage::Unpair => {
                inner.remove_paired(&remote.fingerprint).await;
            }
            ControlMessage::Bye => break,
            other => {
                tracing::debug!(kind = other.kind(), "非预期消息, 断开连接");
                break;
            }
        }
    }
    graceful_close(&mut stream).await;
}

/// 构造"意外消息"错误
fn unexpected(expected: &'static str, got: &ControlMessage) -> SyncError {
    SyncError::Protocol(protocol::ProtocolError::Unexpected {
        expected,
        got: got.kind().to_string(),
    })
}
