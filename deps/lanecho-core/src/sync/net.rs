//! sync network sessions: outbound transactions and the accept loop; above the
//! frame protocol, below the engine's decisions.
//!
//! The connection model is dial, transact, leave: every transaction opens a
//! fresh TLS connection, passes the Hello gate (version plus fingerprint
//! agreement), runs the transaction, and finishes with Bye plus
//! graceful_close. The engine's decisions (pairing verdicts, the sync check
//! chain) live in [`super::Inner`]'s methods; this module only orchestrates IO.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::PROTOCOL_VERSION;
use crate::config::{
    CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT, IDLE_TIMEOUT, MAX_CONCURRENT_CONNECTIONS,
    PAIR_DECISION_TIMEOUT, REPLY_TIMEOUT,
};
use crate::discovery::Peer;
use crate::identity::DeviceIdentity;
use crate::protocol::{self, ControlMessage, PeerInfo};
use crate::tls;

use super::{BlobOffer, BlobSource, Inner, OfferDecision, SyncError, blob};
use crate::protocol::{FileMeta, FilesOfferMeta, ImageOfferMeta, content_type, reason_code};

/// Close a connection: shutdown, then drain to EOF. Closing outright while
/// unread data sits in the receive buffer triggers an RST that wipes out
/// frames still in flight.
pub(super) async fn graceful_close<S>(stream: &mut S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = stream.shutdown().await;
    let mut drain = [0u8; 256];
    // Wait at most 3 seconds on an uncooperative peer, so shutdown cannot hang
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

/// Dial a peer: try each candidate address over TCP, with TLS strictly pinned
/// to the peer's certificate fingerprint
///
/// `config` is cached and reused by the engine per peer fingerprint (see
/// `Inner::client_tls_for`), so a dial no longer rebuilds it and re-parses the
/// local certificate and private key DER every time.
async fn connect_peer(
    config: Arc<rustls::ClientConfig>,
    peer: &Peer,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, SyncError> {
    let connector = TlsConnector::from(config);
    // ServerName only exists to satisfy the API — a constant input cannot
    // fail; verification goes through the fingerprint pin (see the tls module)
    let name = rustls_pki_types::ServerName::try_from("lanecho")
        .map_err(|_| SyncError::PeerUnreachable)?;
    for addr in &peer.addrs {
        let Ok(Ok(tcp)) =
            tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((*addr, peer.port))).await
        else {
            continue;
        };
        let _ = tcp.set_nodelay(true);
        match connector.connect(name.clone(), tcp).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                tracing::debug!(%addr, "TLS 连接失败, 尝试下一候选地址: {e}");
            }
        }
    }
    Err(SyncError::PeerUnreachable)
}

/// Outbound handshake: Hello → HelloAck, checking the version and that the
/// declared fingerprint equals the pinned one
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
    // The TLS layer already guarantees the certificate matches the pinned
    // fingerprint; this additionally guarantees the declaration agrees with
    // the certificate, which is what blocks impersonation
    if info.fingerprint != expected_fp {
        return Err(SyncError::FingerprintMismatch);
    }
    Ok(info)
}

/// Sync transaction (dialer side): deliver one piece of clipboard text and
/// wait for the peer's verdict
///
/// `sync_frame` is a pre-encoded ClipboardSync frame — a broadcast shares one
/// copy across all targets (see `Inner::broadcast_text`). This function only
/// orchestrates the session and writes the frame out.
pub(super) async fn sync_transaction(
    identity: &DeviceIdentity,
    config: Arc<rustls::ClientConfig>,
    peer: &Peer,
    sync_frame: &[u8],
) -> Result<(), SyncError> {
    let mut stream = connect_peer(config, peer).await?;
    handshake_out(&mut stream, identity, &peer.info.fingerprint).await?;
    protocol::write_raw_frame(&mut stream, sync_frame).await?;
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

/// blob sync transaction (dialer side): offer → one of three branches → raw
/// stream → footer → final verdict
///
/// Against a 1.0 peer the offer is still a valid frame: it sees an unknown
/// content_type and answers `sync_rejected(unsupported_type)`, which this
/// function returns as an ordinary rejection. Backward compatibility rides on
/// the protocol's own rejection path; there is no version sniffing.
pub(super) async fn blob_transaction(
    identity: &DeviceIdentity,
    config: Arc<rustls::ClientConfig>,
    peer: &Peer,
    offer_frame: &[u8],
    metas: &[FileMeta],
    source: BlobSource,
) -> Result<(), SyncError> {
    let mut stream = connect_peer(config, peer).await?;
    handshake_out(&mut stream, identity, &peer.info.fingerprint).await?;
    protocol::write_raw_frame(&mut stream, offer_frame).await?;
    let reply = tokio::time::timeout(REPLY_TIMEOUT, protocol::read_frame(&mut stream))
        .await
        .map_err(|_| SyncError::Timeout("blob_offer_reply"))??;
    let result = match reply {
        // LWW says stale: the peer does not want it, nothing to transfer —
        // keeping the "Ack means success" semantics
        ControlMessage::SyncAck => Ok(()),
        ControlMessage::SyncRejected { reason_code } => Err(SyncError::Rejected(reason_code)),
        ControlMessage::BlobAccept => send_blob_body(&mut stream, metas, source).await,
        other => Err(unexpected("blob_accept", &other)),
    };
    let _ = protocol::write_frame(&mut stream, &ControlMessage::Bye).await;
    graceful_close(&mut stream).await;
    result
}

/// The dialer side's transfer stage: raw blob stream, footer, final reply
async fn send_blob_body<S>(
    stream: &mut S,
    metas: &[FileMeta],
    source: BlobSource,
) -> Result<(), SyncError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hash = match &source {
        BlobSource::Memory(bytes) => blob::send_bytes(stream, bytes).await?,
        BlobSource::Files(paths) => blob::send_files(stream, paths, metas).await?,
    };
    protocol::write_frame(stream, &ControlMessage::BlobFooter { hash }).await?;
    let reply = tokio::time::timeout(REPLY_TIMEOUT, protocol::read_frame(stream))
        .await
        .map_err(|_| SyncError::Timeout("blob_final_reply"))??;
    match reply {
        ControlMessage::SyncAck => Ok(()),
        ControlMessage::SyncRejected { reason_code } => Err(SyncError::Rejected(reason_code)),
        other => Err(unexpected("sync_ack", &other)),
    }
}

/// Pairing transaction (dialer side): send the request and wait for the remote
/// user's decision in the prompt — a human is in the loop, hence the long
/// timeout
pub(super) async fn pair_transaction(
    identity: &DeviceIdentity,
    config: Arc<rustls::ClientConfig>,
    peer: &Peer,
) -> Result<PeerInfo, SyncError> {
    let mut stream = connect_peer(config, peer).await?;
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

/// Unpair notification (dialer side, best effort): failing is fine, the
/// security boundary sits on the receiving side
pub(super) async fn unpair_transaction(
    identity: &DeviceIdentity,
    config: Arc<rustls::ClientConfig>,
    peer: &Peer,
) -> Result<(), SyncError> {
    let mut stream = connect_peer(config, peer).await?;
    handshake_out(&mut stream, identity, &peer.info.fingerprint).await?;
    protocol::write_frame(&mut stream, &ControlMessage::Unpair).await?;
    let _ = protocol::write_frame(&mut stream, &ControlMessage::Bye).await;
    graceful_close(&mut stream).await;
    Ok(())
}

/// Accept loop: take inbound connections and refuse outright anything beyond
/// the concurrency cap, which is what keeps slow-loris out
pub(super) async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    let acceptor = TlsAcceptor::from(Arc::clone(&inner.server_tls));
    let limiter = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        let Ok((tcp, addr)) = listener.accept().await else {
            // A transient accept error (EMFILE and friends): yield a beat,
            // then retry
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

/// Serve one connection: TLS plus the Hello gate, both under a deadline, then
/// into the transaction loop
async fn serve_conn(inner: Arc<Inner>, acceptor: TlsAcceptor, tcp: TcpStream) {
    let _ = tcp.set_nodelay(true);
    // The whole unauthenticated stage is under one deadline, which shuts out
    // connections that occupy a slot and then say nothing
    let gate = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut stream = acceptor.accept(tcp).await?;
        let cert_fp = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())
            .ok_or(SyncError::FingerprintMismatch)?;
        let first = protocol::read_frame(&mut stream).await?;
        let ControlMessage::Hello { version, info } = first else {
            return Err(unexpected("hello", &first));
        };
        protocol::check_version(&version)?;
        // The declared fingerprint must agree with the TLS certificate, which
        // is what blocks impersonation
        if info.fingerprint != cert_fp {
            return Err(SyncError::FingerprintMismatch);
        }
        protocol::write_frame(
            &mut stream,
            &ControlMessage::HelloAck {
                version: PROTOCOL_VERSION.to_string(),
                info: inner.current_identity().peer_info(),
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

    // Transaction loop: a peer usually runs one transaction and says Bye; a
    // read failure (dropped connection) goes straight to cleanup.
    // The gap between frames is bounded so a half-open connection cannot hold
    // a slot forever. PairRequest's 300s human-in-the-loop wait happens inside
    // decide_pair and is unaffected by this timeout.
    loop {
        let Ok(Ok(msg)) =
            tokio::time::timeout(IDLE_TIMEOUT, protocol::read_frame(&mut stream)).await
        else {
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
                content_type: kind,
                data,
                ..
            } => {
                // Images and files arrive as a blob offer: three-branch
                // handling followed by stream reception. Text and unknown
                // types still go through the single-frame check chain
                if kind == content_type::IMAGE || kind == content_type::FILES {
                    if serve_blob(&inner, &mut stream, &remote, timestamp_ms, &kind, &data)
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                let reply = match inner.accept_sync(&remote, timestamp_ms, &kind, data).await {
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

/// Receiver-side orchestration of a blob offer: decide → receive the stream →
/// verify → land it → final reply
///
/// Err means the connection cannot continue (IO break or protocol violation)
/// and the caller leaves the transaction loop. A business-level refusal (type
/// toggled off, over the cap, checksum mismatch) is answered with
/// SyncRejected and returns Ok.
async fn serve_blob<S>(
    inner: &Arc<Inner>,
    stream: &mut S,
    remote: &PeerInfo,
    timestamp_ms: u64,
    kind: &str,
    data: &str,
) -> Result<(), SyncError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Failing to parse the metadata is a protocol violation — a 1.1 dialer
    // never sends bad meta — so drop the connection
    let offer = if kind == content_type::IMAGE {
        BlobOffer::Image(
            serde_json::from_str::<ImageOfferMeta>(data)
                .map_err(|e| SyncError::Protocol(protocol::ProtocolError::Codec(e)))?,
        )
    } else {
        BlobOffer::Files(
            serde_json::from_str::<FilesOfferMeta>(data)
                .map_err(|e| SyncError::Protocol(protocol::ProtocolError::Codec(e)))?,
        )
    };
    match inner.decide_blob_offer(remote, timestamp_ms, &offer) {
        OfferDecision::Reject(code) => {
            protocol::write_frame(
                stream,
                &ControlMessage::SyncRejected {
                    reason_code: code.to_string(),
                },
            )
            .await?;
            return Ok(());
        }
        OfferDecision::Stale => {
            protocol::write_frame(stream, &ControlMessage::SyncAck).await?;
            return Ok(());
        }
        OfferDecision::Accept => {}
    }
    protocol::write_frame(stream, &ControlMessage::BlobAccept).await?;

    // Receive the stream, counting exactly the total the offer declared. Each
    // read carries a timeout, so a half-open connection does not hold a slot
    match offer {
        BlobOffer::Image(meta) => {
            let (png, actual_hash) = blob::recv_bytes(stream, meta.total_bytes).await?;
            let footer = expect_footer(stream).await?;
            let reply = if footer != actual_hash {
                tracing::warn!(from = %remote.name, "图像流校验不符, 丢弃");
                ControlMessage::SyncRejected {
                    reason_code: reason_code::CHECKSUM_MISMATCH.to_string(),
                }
            } else {
                match inner.finish_blob_image(remote, timestamp_ms, png).await {
                    Ok(()) => ControlMessage::SyncAck,
                    Err(code) => ControlMessage::SyncRejected {
                        reason_code: code.to_string(),
                    },
                }
            };
            protocol::write_frame(stream, &reply).await?;
        }
        BlobOffer::Files(meta) => {
            // Batch directory: on a failed checksum or a mid-transfer break
            // the whole directory is removed, leaving nothing half-written
            let batch_dir = inner
                .data_dir()
                .join(blob::SYNC_FILES_DIR)
                .join(&uuid::Uuid::new_v4().simple().to_string()[..8]);
            let received = blob::recv_files(stream, &meta.files, &batch_dir).await;
            let (parts, actual_hash) = match received {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tokio::fs::remove_dir_all(&batch_dir).await;
                    return Err(e);
                }
            };
            let footer = match expect_footer(stream).await {
                Ok(hash) => hash,
                Err(e) => {
                    let _ = tokio::fs::remove_dir_all(&batch_dir).await;
                    return Err(e);
                }
            };
            let reply = if footer != actual_hash {
                tracing::warn!(from = %remote.name, "文件流校验不符, 丢弃整批");
                let _ = tokio::fs::remove_dir_all(&batch_dir).await;
                ControlMessage::SyncRejected {
                    reason_code: reason_code::CHECKSUM_MISMATCH.to_string(),
                }
            } else {
                match inner
                    .finish_blob_files(remote, timestamp_ms, parts, &meta.files, &batch_dir)
                    .await
                {
                    Ok(()) => ControlMessage::SyncAck,
                    Err(code) => {
                        let _ = tokio::fs::remove_dir_all(&batch_dir).await;
                        ControlMessage::SyncRejected {
                            reason_code: code.to_string(),
                        }
                    }
                }
            };
            protocol::write_frame(stream, &reply).await?;
        }
    }
    Ok(())
}

/// Read and validate the BlobFooter frame, returning the hash it carries
async fn expect_footer<S>(stream: &mut S) -> Result<String, SyncError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let msg = tokio::time::timeout(IDLE_TIMEOUT, protocol::read_frame(stream))
        .await
        .map_err(|_| SyncError::Timeout("blob_footer"))??;
    match msg {
        ControlMessage::BlobFooter { hash } => Ok(hash),
        other => Err(unexpected("blob_footer", &other)),
    }
}

/// Build an "unexpected message" error
fn unexpected(expected: &'static str, got: &ControlMessage) -> SyncError {
    SyncError::Protocol(protocol::ProtocolError::Unexpected {
        expected,
        got: got.kind().to_string(),
    })
}
