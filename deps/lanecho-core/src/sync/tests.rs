//! sync 引擎回环测试: 同进程双引擎, 真 TLS + 真协议 + 真发现
//! (harness 模式承袭 deskmate transfer/tests.rs)
//!
//! 每个测试使用独立的 UDP 发现端口, 避免并行测试互相污染;
//! 引擎按指纹过滤目标, 同网段偶入的无关节点(其他测试/真实设备)无影响。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::clipboard::{ClipboardContent, ClipboardEvent, now_ms};
use crate::config::ECHO_RECENT_CAP;

use super::*;

/// 独立临时目录, Drop 时自动清理
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("lanecho-sync-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 测试节点: 引擎 + 事件流 + 剪贴板注入口
struct TestNode {
    engine: Arc<SyncEngine>,
    events: mpsc::Receiver<EngineEvent>,
    clip_tx: mpsc::Sender<ClipboardEvent>,
    fingerprint: String,
    _dir: TempDir,
}

/// 启动一个测试节点(随机 TCP 端口, 指定发现端口)
async fn start_node(discovery_port: u16) -> TestNode {
    let dir = TempDir::new();
    let (clip_tx, clip_rx) = mpsc::channel(16);
    let (engine, events) = SyncEngine::start(
        EngineConfig {
            data_dir: dir.0.clone(),
            tcp_port: 0,
            discovery_port,
            passive: false,
            sync_enabled: true,
        },
        clip_rx,
    )
    .await
    .expect("引擎启动失败");
    let fingerprint = engine.local_info().fingerprint.clone();
    TestNode {
        engine: Arc::new(engine),
        events,
        clip_tx,
        fingerprint,
        _dir: dir,
    }
}

/// 等待指定指纹的节点出现在发现表中
async fn wait_discover(engine: &SyncEngine, fingerprint: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if engine
                .peers()
                .iter()
                .any(|p| p.info.fingerprint == fingerprint)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("等待节点互相发现超时");
}

/// 从事件流中等待并提取符合条件的事件(其余事件消费丢弃)
async fn wait_event<T>(
    events: &mut mpsc::Receiver<EngineEvent>,
    pick: impl Fn(&EngineEvent) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.expect("事件通道意外关闭");
            if let Some(value) = pick(&event) {
                return value;
            }
        }
    })
    .await
    .expect("等待事件超时")
}

/// 断言窗口期内不出现 ApplyRemote(其余事件放行)
async fn assert_no_apply(events: &mut mpsc::Receiver<EngineEvent>, window: Duration) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) => {
                assert!(
                    !matches!(event, EngineEvent::ApplyRemote { .. }),
                    "不应收到 ApplyRemote: {event:?}"
                );
            }
            _ => return,
        }
    }
}

/// 注入一条模拟"本机复制"的文本事件
async fn inject_text(node: &TestNode, text: &str, timestamp_ms: u64) {
    let content = ClipboardContent::Text(text.to_string());
    let hash = content.hash();
    node.clip_tx
        .send(ClipboardEvent {
            content,
            hash,
            timestamp_ms,
        })
        .await
        .expect("注入剪贴板事件失败");
}

/// 建立 A→B 的配对(B 自动接受), 消化双方的 Paired 事件
async fn establish_pair(a: &mut TestNode, b: &mut TestNode) {
    wait_discover(&a.engine, &b.fingerprint).await;
    wait_discover(&b.engine, &a.fingerprint).await;
    let pair_task = tokio::spawn({
        let engine = Arc::clone(&a.engine);
        let target = b.fingerprint.clone();
        async move { engine.pair(&target).await }
    });
    let requester = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::PairRequested { peer } => Some(peer.fingerprint.clone()),
        _ => None,
    })
    .await;
    assert_eq!(requester, a.fingerprint);
    b.engine.respond_pair(&requester, true);
    pair_task
        .await
        .expect("配对任务崩溃")
        .expect("配对应当成功");
    // 消化双方的 Paired 事件, 避免影响后续断言
    wait_event(&mut a.events, |ev| {
        matches!(ev, EngineEvent::Paired { .. }).then_some(())
    })
    .await;
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::Paired { .. }).then_some(())
    })
    .await;
}

/// 核心链路: 发现 → 配对 → 文本互同步(逐字节一致, 含首尾空白与控制字符)
#[tokio::test]
async fn pair_and_sync_roundtrip() {
    let mut a = start_node(42611).await;
    let mut b = start_node(42611).await;
    establish_pair(&mut a, &mut b).await;

    // 刻意包含首尾空白/换行/emoji: 逐字节一致是硬约束
    let text = "  你好 lanecho\n\t🚀 尾巴  ";
    inject_text(&a, text, now_ms()).await;

    let (applied, from) = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::ApplyRemote { text, from, .. } => {
            Some((text.clone(), from.fingerprint.clone()))
        }
        _ => None,
    })
    .await;
    assert_eq!(applied, text, "同步文本必须逐字节一致");
    assert_eq!(from, a.fingerprint);

    // 发送侧应收到成功回执
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    assert!(result.is_ok(), "同步回执应为成功: {result:?}");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// 对端拒绝配对: 发起方得到 PairRejected, 双方均不建立配对
#[tokio::test]
async fn pair_rejection_propagates() {
    let a = start_node(42612).await;
    let mut b = start_node(42612).await;
    wait_discover(&a.engine, &b.fingerprint).await;

    let pair_task = tokio::spawn({
        let engine = Arc::clone(&a.engine);
        let target = b.fingerprint.clone();
        async move { engine.pair(&target).await }
    });
    let requester = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::PairRequested { peer } => Some(peer.fingerprint.clone()),
        _ => None,
    })
    .await;
    b.engine.respond_pair(&requester, false);
    let result = pair_task.await.expect("配对任务崩溃");
    assert!(matches!(result, Err(SyncError::PairRejected)));
    assert!(a.engine.paired_list().is_empty());
    assert!(b.engine.paired_list().is_empty());

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// 接收侧检查链: 对端暂停同步(熔断)时拒绝并回结构化拒因
#[tokio::test]
async fn disabled_receiver_rejects_with_reason() {
    let mut a = start_node(42613).await;
    let mut b = start_node(42613).await;
    establish_pair(&mut a, &mut b).await;

    b.engine.set_sync_enabled(false);
    inject_text(&a, "should be rejected", now_ms()).await;

    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    let err = result.expect_err("对端已熔断, 同步应被拒绝");
    assert!(err.contains("disabled"), "拒因应含 disabled: {err}");
    assert_no_apply(&mut b.events, Duration::from_millis(800)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// 回声抑制: 远端写入绕回本机剪贴板后, 不得再次广播(不形成环路)
#[tokio::test]
async fn echo_is_suppressed() {
    let mut a = start_node(42614).await;
    let mut b = start_node(42614).await;
    establish_pair(&mut a, &mut b).await;

    inject_text(&a, "echo-test", now_ms()).await;
    let applied = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::ApplyRemote { text, .. } => Some(text.clone()),
        _ => None,
    })
    .await;

    // 模拟 B 的装配层写入系统剪贴板后, watcher 检测到该变化并上报
    inject_text(&b, &applied, now_ms()).await;

    // B 的引擎应吞掉回声: A 不得收到"来自 B"的 ApplyRemote
    assert_no_apply(&mut a.events, Duration::from_millis(1200)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// LWW 决胜: 远端时间戳不新于本机最近复制时, 收下但不覆盖剪贴板
#[tokio::test]
async fn lww_ignores_stale_remote() {
    let mut a = start_node(42615).await;
    let mut b = start_node(42615).await;
    establish_pair(&mut a, &mut b).await;

    // B 本机先复制(时间基准), 其广播被 A 正常接收(消费掉)
    let base = now_ms();
    inject_text(&b, "b-fresh", base).await;
    wait_event(&mut a.events, |ev| {
        matches!(ev, EngineEvent::ApplyRemote { .. }).then_some(())
    })
    .await;

    // A 注入一条"更旧"的复制(时钟偏差/迟到场景): B 应回 Ack 但不应用
    inject_text(&a, "a-stale", base.saturating_sub(10_000)).await;
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    assert!(result.is_ok(), "LWW 忽略对发送方透明, 回执应为成功");
    assert_no_apply(&mut b.events, Duration::from_millis(800)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// 解除配对: 本地立即移除, 对端经 Unpair 通知同步移除
#[tokio::test]
async fn unpair_propagates_to_peer() {
    let mut a = start_node(42616).await;
    let mut b = start_node(42616).await;
    establish_pair(&mut a, &mut b).await;

    a.engine.unpair(&b.fingerprint).await;
    assert!(a.engine.paired_list().is_empty());

    let removed = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::Unpaired { fingerprint } => Some(fingerprint.clone()),
        _ => None,
    })
    .await;
    assert_eq!(removed, a.fingerprint);
    assert!(b.engine.paired_list().is_empty());

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// 配对关系持久化: 引擎重启(同数据目录)后配对仍在
#[tokio::test]
async fn paired_survives_restart() {
    let mut a = start_node(42617).await;
    let mut b = start_node(42617).await;
    establish_pair(&mut a, &mut b).await;

    let a_dir = a._dir.0.clone();
    let b_fp = b.fingerprint.clone();
    a.engine.shutdown().await;
    // 落盘是 fire-and-forget 的 blocking 任务, 给它一拍完成
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (clip_tx, clip_rx) = mpsc::channel(16);
    drop(clip_tx);
    let (engine, _events) = SyncEngine::start(
        EngineConfig {
            data_dir: a_dir,
            tcp_port: 0,
            discovery_port: 42617,
            passive: true,
            sync_enabled: true,
        },
        clip_rx,
    )
    .await
    .expect("重启引擎失败");
    assert!(
        engine.paired_list().iter().any(|p| p.fingerprint == b_fp),
        "重启后配对关系应存续"
    );
    engine.shutdown().await;
    b.engine.shutdown().await;
}

/// 回声表语义(纯数据结构): 一次性消费与容量淘汰
#[test]
fn echo_semantics() {
    let mut echo: VecDeque<String> = VecDeque::new();
    for i in 0..(ECHO_RECENT_CAP + 2) {
        if echo.len() >= ECHO_RECENT_CAP {
            echo.pop_front();
        }
        echo.push_back(format!("h{i}"));
    }
    assert_eq!(echo.len(), ECHO_RECENT_CAP);
    assert!(!echo.contains(&"h0".to_string()));
    assert!(!echo.contains(&"h1".to_string()));
    let target = "h5".to_string();
    let hit = echo
        .iter()
        .position(|h| *h == target)
        .map(|i| echo.remove(i));
    assert!(hit.is_some());
    assert!(!echo.contains(&target));
}
