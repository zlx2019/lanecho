//! Subcommand implementations: engine wiring and the interactive loop.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tokio::sync::mpsc;

use lanecho_core::clipboard::{self, ClipboardContent, ClipboardEvent, now_ms};
use lanecho_core::discovery::{DiscoveryService, Peer};
use lanecho_core::identity::DeviceIdentity;
use lanecho_core::sync::{EngineConfig, EngineEvent, SyncEngine, SyncMode, SyncTypes};
use lanecho_core::{DEFAULT_DISCOVERY_PORT, PROTOCOL_VERSION};

use crate::CommonArgs;
use crate::output;

/// The watcher's "read images" toggle: the CLI has no history settings, so it
/// stays on, matching existing behaviour
fn always_read_images() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true))
}

/// The watcher's dedup baseline reset signal: the CLI has no record toggle, so
/// it never fires
fn never_reset() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
}

/// Print the local identity, creating one on the spot if none exists
pub async fn cmd_id(common: &CommonArgs) -> Result<()> {
    let identity = DeviceIdentity::load_or_create(&common.data_dir)?;
    println!("名称:   {}", identity.display_name);
    println!("设备ID: {}", identity.device_id);
    println!("指纹:   {}", identity.fingerprint);
    println!("协议:   lanecho/{PROTOCOL_VERSION}");
    println!("数据目录: {}", common.data_dir.display());
    Ok(())
}

/// Resident node: discovery + pairing + clipboard sync + stdin interaction
pub async fn cmd_listen(
    common: &CommonArgs,
    port: u16,
    auto_accept: bool,
    use_clipboard: bool,
) -> Result<()> {
    let (clip_tx, clip_rx) = mpsc::channel(16);
    let (engine, mut events) = SyncEngine::start(
        EngineConfig {
            data_dir: common.data_dir.clone(),
            tcp_port: port,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            passive: false,
            sync_mode: SyncMode::Both,
            sync_types: SyncTypes {
                text: true,
                images: true,
                files: true,
            },
            max_sync_file_bytes: 32 * 1024 * 1024,
        },
        clip_rx,
    )
    .await?;
    // Wrapped in an Arc: /pair starts pairing from its own task. Waiting for
    // the peer to confirm can take up to 5 minutes and must not block the
    // event loop, or two ends running /pair at once would never see each
    // other's request.
    let engine = std::sync::Arc::new(engine);
    let info = engine.local_info();
    output::ok(&format!(
        "本机 {} [{}] 已上线, 监听端口 {}",
        info.name,
        output::fp8(&info.fingerprint),
        engine.port()
    ));
    if use_clipboard {
        clipboard::spawn_watcher(clip_tx.clone(), always_read_images(), never_reset());
        output::info("已接管系统剪贴板监视: 本机复制将广播给已配对节点");
    } else {
        output::info("未接管系统剪贴板(--no-clipboard): 输入一行文本可模拟复制");
    }
    if auto_accept {
        output::info("配对请求将被自动接受(--yes)");
    }

    let mut stdin = Some(spawn_stdin_reader());
    // Fingerprint of the most recent pending pairing request (what /y and /n
    // act on)
    let mut pending_pair: Option<String> = None;
    loop {
        tokio::select! {
            maybe_event = events.recv() => {
                let Some(event) = maybe_event else { break };
                handle_event(event, &engine, auto_accept, use_clipboard, &mut pending_pair).await;
            }
            maybe_line = next_line(&mut stdin), if stdin.is_some() => {
                match maybe_line {
                    Some(line) => {
                        if !handle_line(&line, &engine, &clip_tx, &mut pending_pair).await {
                            break;
                        }
                    }
                    // stdin closed (pipe, or running in the background): stay
                    // resident and only disable the input branch
                    None => stdin = None,
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    output::info("正在下线...");
    engine.shutdown().await;
    Ok(())
}

/// stdin line reader: a dedicated std thread plus a channel.
///
/// Deliberately avoids `tokio::io::stdin()`: it is a blocking read on the
/// blocking thread pool, and runtime shutdown waits for that read to return,
/// so after Ctrl+C the process hangs waiting for one last line (a limitation
/// tokio documents). A plain std thread does not occupy the blocking pool and
/// is reclaimed by the OS when the process exits.
fn spawn_stdin_reader() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(8);
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
        // EOF, or the consumer went away: the sender drops and the async side
        // receives None
    });
    rx
}

/// Read the next stdin line; the Option wrapper suits select's conditional
/// branch
async fn next_line(stdin: &mut Option<mpsc::Receiver<String>>) -> Option<String> {
    match stdin {
        Some(rx) => rx.recv().await,
        None => None,
    }
}

/// Handle one engine event
async fn handle_event(
    event: EngineEvent,
    engine: &SyncEngine,
    auto_accept: bool,
    use_clipboard: bool,
    pending_pair: &mut Option<String>,
) {
    match event {
        EngineEvent::PeerUp(peer) => {
            output::event(
                "▲",
                &format!(
                    "上线 {} [{}] {:?}",
                    peer.info.name,
                    output::fp8(&peer.info.fingerprint),
                    peer.addrs
                ),
            );
        }
        EngineEvent::PeerDown(fp) => {
            output::event("▼", &format!("下线 [{}]", output::fp8(&fp)));
        }
        EngineEvent::PairRequested { peer } => {
            if auto_accept {
                engine.respond_pair(&peer.fingerprint, true);
                output::ok(&format!(
                    "已自动接受 {} 的配对请求(指纹 {})",
                    peer.name,
                    output::fp8(&peer.fingerprint)
                ));
            } else {
                output::warn(&format!(
                    "{} 请求配对, 指纹 {} —— 输入 /y 接受, /n 拒绝",
                    peer.name,
                    output::fp8(&peer.fingerprint)
                ));
                *pending_pair = Some(peer.fingerprint);
            }
        }
        EngineEvent::Paired { peer } => {
            output::ok(&format!(
                "已与 {} 配对 [{}]",
                peer.name,
                output::fp8(&peer.fingerprint)
            ));
        }
        EngineEvent::Unpaired { fingerprint } => {
            output::event("✂", &format!("配对已解除 [{}]", output::fp8(&fingerprint)));
        }
        EngineEvent::LocalCopied { content, .. } => {
            let desc = match &content {
                ClipboardContent::Text(t) => output::preview(t),
                ClipboardContent::Image { width, height, .. } => {
                    format!("图像 {width}x{height}")
                }
                ClipboardContent::Files(paths) => format!("文件 x{}", paths.len()),
                // The CLI always reads images; ImageUnread never occurs
                ClipboardContent::ImageUnread => "图像(未读取)".to_string(),
            };
            output::event("⇡", &format!("本机复制 [{}] {desc}", content.kind()));
        }
        EngineEvent::ApplyRemote { content, from, .. } => match content {
            ClipboardContent::Text(text) => {
                output::event(
                    "⇣",
                    &format!("来自 {} 的剪贴板: {}", from.name, output::preview(&text)),
                );
                if use_clipboard && let Err(e) = clipboard::write_text(text).await {
                    output::warn(&format!("写入系统剪贴板失败: {e}"));
                }
            }
            ClipboardContent::Image {
                width,
                height,
                rgba,
            } => {
                output::event("⇣", &format!("来自 {} 的图像 {width}×{height}", from.name));
                if use_clipboard && let Err(e) = clipboard::write_image(width, height, rgba).await {
                    output::warn(&format!("写入系统剪贴板失败: {e}"));
                }
            }
            ClipboardContent::Files(paths) => {
                output::event("⇣", &format!("来自 {} 的文件 ×{}", from.name, paths.len()));
                if use_clipboard && let Err(e) = clipboard::write_files(paths).await {
                    output::warn(&format!("写入系统剪贴板失败: {e}"));
                }
            }
            other => {
                output::warn(&format!("未知的远端内容类型: {}", other.kind()));
            }
        },
        EngineEvent::SyncSent { to, result } => match result {
            Ok(()) => output::event("→", &format!("已同步至 {}", to.name)),
            Err(e) => output::warn(&format!("同步至 {} 失败: {e}", to.name)),
        },
    }
}

/// Handle one line of stdin input; returning false means quit
async fn handle_line(
    line: &str,
    engine: &std::sync::Arc<SyncEngine>,
    clip_tx: &mpsc::Sender<ClipboardEvent>,
    pending_pair: &mut Option<String>,
) -> bool {
    match line.trim() {
        "" => {}
        "/quit" => return false,
        cmd if cmd.starts_with("/pair ") => {
            let target = cmd.trim_start_matches("/pair ").trim();
            match find_target(&engine.peers(), target) {
                Some(peer) => {
                    output::info(&format!(
                        "向 {} 发起配对, 等待对方确认(可继续操作)...",
                        peer.info.name
                    ));
                    // Pairing completes inside the resident process, so the
                    // result takes effect immediately (in memory and on disk)
                    let engine = std::sync::Arc::clone(engine);
                    tokio::spawn(async move {
                        match engine.pair(&peer.info.fingerprint).await {
                            Ok(()) => {}
                            Err(e) => output::warn(&format!("配对失败: {e}")),
                        }
                    });
                }
                None => output::warn(&format!(
                    "在线节点中未找到 {target}(可用 /peers 查看; 支持 名称/设备ID/指纹前缀)"
                )),
            }
        }
        "/y" | "/n" => {
            let accept = line.trim() == "/y";
            match pending_pair.take() {
                Some(fp) => {
                    engine.respond_pair(&fp, accept);
                    if !accept {
                        output::info("已拒绝配对请求");
                    }
                }
                None => output::warn("当前没有待决的配对请求"),
            }
        }
        "/peers" => {
            let peers = engine.peers();
            if peers.is_empty() {
                output::info("暂无在线节点");
            }
            for p in peers {
                println!(
                    "  {} [{}] {:?} :{}",
                    p.info.name,
                    output::fp8(&p.info.fingerprint),
                    p.addrs,
                    p.port
                );
            }
        }
        "/paired" => {
            let list = engine.paired_list();
            if list.is_empty() {
                output::info("尚未与任何设备配对");
            }
            for p in list {
                println!("  {} [{}]", p.name, output::fp8(&p.fingerprint));
            }
        }
        cmd if cmd.starts_with("/image") => {
            // Inject an image with a deterministic pattern (simulates a copy
            // and broadcasts it; this is the peer-side tool for interop tests
            // and protocol debugging). The formula mirrors the Swift interop
            // test: per pixel r=(p*7)%251 g=(p*11)%241 b=(p*13)%239 a=255.
            // **Every pixel must be fully opaque**: the receiving side
            // (ImageIO) decodes with premultiplied alpha, which rewrites the
            // bytes of translucent pixels and breaks any byte-for-byte
            // assertion.
            let dims = cmd.trim_start_matches("/image").trim();
            let (width, height) = parse_dims(dims).unwrap_or((64, 48));
            let mut rgba = Vec::with_capacity(width * height * 4);
            for pixel in 0..width * height {
                rgba.extend_from_slice(&[
                    (pixel * 7 % 251) as u8,
                    (pixel * 11 % 241) as u8,
                    (pixel * 13 % 239) as u8,
                    0xFF,
                ]);
            }
            let content = ClipboardContent::Image {
                width,
                height,
                rgba,
            };
            let hash = content.hash();
            let event = ClipboardEvent::new(content, hash, now_ms());
            if clip_tx.send(event).await.is_ok() {
                output::info(&format!("已注入图像 {width}x{height}(模拟复制)"));
            }
        }
        cmd if cmd.starts_with("/file ") => {
            // Inject file references (simulates a copy and broadcasts it);
            // paths are split on whitespace. Interop tests never use paths with
            // spaces, so no escaping is supported.
            let paths: Vec<std::path::PathBuf> = cmd
                .trim_start_matches("/file ")
                .split_whitespace()
                .map(std::path::PathBuf::from)
                .collect();
            if paths.is_empty() {
                output::warn("用法: /file <路径>...");
            } else {
                let content = ClipboardContent::Files(paths);
                let hash = content.hash();
                let event = ClipboardEvent::new(content, hash, now_ms());
                if clip_tx.send(event).await.is_ok() {
                    output::info("已注入文件(模拟复制)");
                }
            }
        }
        cmd if cmd.starts_with('/') => {
            output::warn(
                "未知命令; 可用: /pair <目标> /image [宽x高] /file <路径>... /y /n /peers /paired /quit",
            );
        }
        _ => {
            // Inject a text event (simulates one copy): it takes exactly the
            // same engine path as a real clipboard change
            let content = ClipboardContent::Text(line.to_string());
            let hash = content.hash();
            let event = ClipboardEvent::new(content, hash, now_ms());
            if clip_tx.send(event).await.is_ok() {
                output::info("已注入文本(模拟复制)");
            }
        }
    }
    true
}

/// Passive scan: listen without broadcasting, print the peers discovered
pub async fn cmd_scan(common: &CommonArgs, wait_secs: u64) -> Result<()> {
    let identity = DeviceIdentity::load_or_create(&common.data_dir)?;
    let (discovery, mut events) =
        DiscoveryService::start(&identity, 0, DEFAULT_DISCOVERY_PORT, true).await?;
    output::info(&format!("扫描中({wait_secs}s)..."));
    let deadline = tokio::time::sleep(Duration::from_secs(wait_secs));
    tokio::pin!(deadline);
    let mut count = 0u32;
    loop {
        tokio::select! {
            maybe_event = events.recv() => {
                if let Some(lanecho_core::discovery::PeerEvent::Up(peer)) = maybe_event {
                    count += 1;
                    println!(
                        "  {} [{}] {:?} :{} ({})",
                        peer.info.name,
                        output::fp8(&peer.info.fingerprint),
                        peer.addrs,
                        peer.port,
                        peer.info.platform
                    );
                }
            }
            _ = &mut deadline => break,
        }
    }
    discovery.shutdown().await;
    output::ok(&format!("共发现 {count} 个节点"));
    Ok(())
}

/// Active pairing: find the target peer, send a pairing request, wait for the
/// peer to confirm
pub async fn cmd_pair(common: &CommonArgs, target: &str, wait_secs: u64) -> Result<()> {
    let (_clip_tx, clip_rx) = mpsc::channel(1);
    let (engine, mut events) = SyncEngine::start(
        EngineConfig {
            data_dir: common.data_dir.clone(),
            tcp_port: 0,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            passive: true,
            sync_mode: SyncMode::Off,
            sync_types: SyncTypes::default(),
            max_sync_file_bytes: 32 * 1024 * 1024,
        },
        clip_rx,
    )
    .await?;
    output::info(&format!("搜索目标 {target}(最多 {wait_secs}s)..."));
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    let peer = loop {
        if let Some(peer) = find_target(&engine.peers(), target) {
            break peer;
        }
        if Instant::now() >= deadline {
            engine.shutdown().await;
            bail!("未找到目标节点: {target}");
        }
        // Drain events so the channel does not back up, and yield while waiting
        tokio::select! {
            _ = events.recv() => {}
            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    };
    output::info(&format!(
        "向 {} 发起配对, 等待对方确认 —— 对端应核对指纹: {}",
        peer.info.name,
        output::fp8(&engine.local_info().fingerprint)
    ));
    let result = engine.pair(&peer.info.fingerprint).await;
    engine.shutdown().await;
    match result {
        Ok(()) => {
            output::ok(&format!("已与 {} 配对", peer.info.name));
            output::info(
                "注意: 若本机已有常驻 listen 进程, 它不会热加载新配对 —— \
                 重启该 listen, 或直接在其中用 /pair <目标> 配对",
            );
            Ok(())
        }
        Err(e) => bail!("配对失败: {e}"),
    }
}

/// Parse a "WIDTHxHEIGHT" size such as "64x48"; both parts must be positive
/// integers
fn parse_dims(raw: &str) -> Option<(usize, usize)> {
    let (w, h) = raw.split_once(['x', 'X', '×'])?;
    let width = w.trim().parse().ok().filter(|v| *v > 0)?;
    let height = h.trim().parse().ok().filter(|v| *v > 0)?;
    Some((width, height))
}

/// Find a target peer by exact name, exact device id, or fingerprint prefix
/// (every identifier the id command prints works)
fn find_target(peers: &[Peer], target: &str) -> Option<Peer> {
    peers
        .iter()
        .find(|p| {
            p.info.name == target
                || p.info.device_id == target
                || p.info.fingerprint.starts_with(target)
        })
        .cloned()
}

/// Watch the local clipboard and print change events of every type (no
/// networking)
pub async fn cmd_watch() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(16);
    clipboard::spawn_watcher(tx, always_read_images(), never_reset());
    output::info("剪贴板监视中(Ctrl+C 退出)...");
    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                let Some(event) = maybe_event else { break };
                let desc = match &event.content {
                    ClipboardContent::Text(t) => output::preview(t),
                    ClipboardContent::Image { width, height, rgba } => {
                        format!("图像 {width}x{height} ({} 字节 RGBA)", rgba.len())
                    }
                    ClipboardContent::Files(paths) => format!("{paths:?}"),
                    // The CLI always reads images; ImageUnread never occurs
                    ClipboardContent::ImageUnread => "图像(未读取)".to_string(),
                };
                output::event(
                    "◆",
                    &format!(
                        "[{}] {desc} hash={}",
                        event.content.kind(),
                        output::fp8(&event.hash)
                    ),
                );
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}
