//! 子命令实现: 引擎装配与交互循环

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tokio::sync::mpsc;

use lanecho_core::clipboard::{self, ClipboardContent, ClipboardEvent, now_ms};
use lanecho_core::discovery::{DiscoveryService, Peer};
use lanecho_core::identity::DeviceIdentity;
use lanecho_core::sync::{EngineConfig, EngineEvent, SyncEngine};
use lanecho_core::{DEFAULT_DISCOVERY_PORT, PROTOCOL_VERSION};

use crate::CommonArgs;
use crate::output;

/// 显示本机身份(不存在时现场生成)
pub async fn cmd_id(common: &CommonArgs) -> Result<()> {
    let identity = DeviceIdentity::load_or_create(&common.data_dir)?;
    println!("名称:   {}", identity.display_name);
    println!("设备ID: {}", identity.device_id);
    println!("指纹:   {}", identity.fingerprint);
    println!("协议:   lanecho/{PROTOCOL_VERSION}");
    println!("数据目录: {}", common.data_dir.display());
    Ok(())
}

/// 驻留节点: 发现 + 配对受理 + 剪贴板互同步 + stdin 交互
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
            sync_enabled: true,
        },
        clip_rx,
    )
    .await?;
    // Arc 包装: /pair 需要在独立任务里发起配对(等待对端确认可长达 5 分钟,
    // 不能阻塞事件循环 —— 否则两端同时 /pair 会互相收不到请求)
    let engine = std::sync::Arc::new(engine);
    let info = engine.local_info();
    output::ok(&format!(
        "本机 {} [{}] 已上线, 监听端口 {}",
        info.name,
        output::fp8(&info.fingerprint),
        engine.port()
    ));
    if use_clipboard {
        clipboard::spawn_watcher(clip_tx.clone());
        output::info("已接管系统剪贴板监视: 本机复制将广播给已配对节点");
    } else {
        output::info("未接管系统剪贴板(--no-clipboard): 输入一行文本可模拟复制");
    }
    if auto_accept {
        output::info("配对请求将被自动接受(--yes)");
    }

    let mut stdin = Some(spawn_stdin_reader());
    // 最近一个待决配对请求的指纹(/y /n 的作用对象)
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
                    // stdin 关闭(管道/后台运行): 保持驻留, 仅停用输入分支
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

/// stdin 行读取器: 独立 std 线程 + 通道
///
/// 刻意不用 `tokio::io::stdin()` —— 它是 blocking 线程池上的阻塞 read,
/// runtime 关闭时会等该 read 返回, 导致 Ctrl+C 后进程挂死在
/// "等最后一行输入"上(tokio 文档明示的限制)。独立 std 线程
/// 不占 blocking 池, 进程退出时由 OS 直接回收。
fn spawn_stdin_reader() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(8);
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
        // EOF 或消费端关闭: 发送端 drop, async 侧 recv 得 None
    });
    rx
}

/// 读下一行 stdin(封装 Option 以配合 select 的条件分支)
async fn next_line(stdin: &mut Option<mpsc::Receiver<String>>) -> Option<String> {
    match stdin {
        Some(rx) => rx.recv().await,
        None => None,
    }
}

/// 处理一条引擎事件
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
            };
            output::event("⇡", &format!("本机复制 [{}] {desc}", content.kind()));
        }
        EngineEvent::ApplyRemote { text, from, .. } => {
            output::event(
                "⇣",
                &format!("来自 {} 的剪贴板: {}", from.name, output::preview(&text)),
            );
            if use_clipboard && let Err(e) = clipboard::write_text(text).await {
                output::warn(&format!("写入系统剪贴板失败: {e}"));
            }
        }
        EngineEvent::SyncSent { to, result } => match result {
            Ok(()) => output::event("→", &format!("已同步至 {}", to.name)),
            Err(e) => output::warn(&format!("同步至 {} 失败: {e}", to.name)),
        },
    }
}

/// 处理一行 stdin 输入; 返回 false 表示退出
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
                    // 配对在常驻进程内完成, 结果即时生效(内存态 + 落盘)
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
        cmd if cmd.starts_with('/') => {
            output::warn("未知命令; 可用: /pair <目标> /y /n /peers /paired /quit");
        }
        _ => {
            // 注入文本事件(模拟一次复制): 走与真实剪贴板完全相同的引擎路径
            let content = ClipboardContent::Text(line.to_string());
            let hash = content.hash();
            let event = ClipboardEvent {
                content,
                hash,
                timestamp_ms: now_ms(),
            };
            if clip_tx.send(event).await.is_ok() {
                output::info("已注入文本(模拟复制)");
            }
        }
    }
    true
}

/// 被动扫描: 只听不广播, 打印发现的节点
pub async fn cmd_scan(common: &CommonArgs, wait_secs: u64) -> Result<()> {
    let identity = DeviceIdentity::load_or_create(&common.data_dir)?;
    let (discovery, mut events) =
        DiscoveryService::start(identity.peer_info(), 0, DEFAULT_DISCOVERY_PORT, true).await?;
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

/// 主动配对: 搜索目标节点并发起配对请求, 等待对方确认
pub async fn cmd_pair(common: &CommonArgs, target: &str, wait_secs: u64) -> Result<()> {
    let (_clip_tx, clip_rx) = mpsc::channel(1);
    let (engine, mut events) = SyncEngine::start(
        EngineConfig {
            data_dir: common.data_dir.clone(),
            tcp_port: 0,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            passive: true,
            sync_enabled: false,
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
        // 消费事件防通道积压, 同时让出等待
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

/// 查找目标节点: 名称精确 / 设备 ID 精确 / 指纹前缀(id 命令展示的标识都可用)
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

/// 监视本机剪贴板并打印全类型变化事件(无网络)
pub async fn cmd_watch() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(16);
    clipboard::spawn_watcher(tx);
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
