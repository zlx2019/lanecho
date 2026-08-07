//! Loopback tests for the sync engine: two engines in one process, real TLS,
//! real protocol, real discovery.
//!
//! Each test uses its own UDP discovery port so parallel tests cannot pollute
//! each other; engines filter targets by fingerprint, so unrelated nodes that
//! wander in from the same subnet (other tests, real devices) have no effect.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::clipboard::{ClipboardContent, ClipboardEvent, now_ms};
use crate::config::{ECHO_RECENT_CAP, ECHO_TTL};

use super::*;

/// An isolated temp directory, removed on Drop
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

/// A test node: engine + event stream + clipboard injection point
struct TestNode {
    engine: Arc<SyncEngine>,
    events: mpsc::Receiver<EngineEvent>,
    clip_tx: mpsc::Sender<ClipboardEvent>,
    fingerprint: String,
    _dir: TempDir,
}

/// Start a test node (random TCP port, given discovery port)
async fn start_node(discovery_port: u16) -> TestNode {
    let dir = TempDir::new();
    let (clip_tx, clip_rx) = mpsc::channel(16);
    let (engine, events) = SyncEngine::start(
        EngineConfig {
            data_dir: dir.0.clone(),
            tcp_port: 0,
            discovery_port,
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

/// Wait until the node with the given fingerprint shows up in the discovery
/// table
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

/// Wait for a matching event on the stream and extract it (every other event
/// is consumed and dropped)
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

/// Pull the text out of ApplyRemote content (the main payload type in the
/// loopback tests)
fn apply_text(content: &ClipboardContent) -> Option<String> {
    match content {
        ClipboardContent::Text(text) => Some(text.clone()),
        _ => None,
    }
}

/// Assert that no ApplyRemote arrives within the window (other events pass)
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

/// Inject a text event that simulates a local copy
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

/// Establish an A→B pairing (B accepts automatically) and consume the Paired
/// event on both sides
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
    // Consume the Paired event on both sides so it cannot disturb later
    // assertions
    wait_event(&mut a.events, |ev| {
        matches!(ev, EngineEvent::Paired { .. }).then_some(())
    })
    .await;
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::Paired { .. }).then_some(())
    })
    .await;
}

/// The core path: discovery → pairing → text sync in both directions
/// (byte-for-byte, including surrounding whitespace and control characters)
#[tokio::test]
async fn pair_and_sync_roundtrip() {
    let mut a = start_node(42611).await;
    let mut b = start_node(42611).await;
    establish_pair(&mut a, &mut b).await;

    // Deliberately carries leading/trailing whitespace, a newline and an
    // emoji: byte-for-byte equality is a hard rule
    let text = "  你好 lanecho\n\t🚀 尾巴  ";
    inject_text(&a, text, now_ms()).await;

    let (applied, from) = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, from, .. } => {
            let text = apply_text(content)?;
            Some((text.clone(), from.fingerprint.clone()))
        }
        _ => None,
    })
    .await;
    assert_eq!(applied, text, "同步文本必须逐字节一致");
    assert_eq!(from, a.fingerprint);

    // The sending side must get a success receipt
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    assert!(result.is_ok(), "同步回执应为成功: {result:?}");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// The peer rejects pairing: the initiator gets PairRejected and neither side
/// records a pairing
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

/// Receiver-side checks: a peer with sync paused rejects the transfer and
/// returns a structured reason
#[tokio::test]
async fn disabled_receiver_rejects_with_reason() {
    let mut a = start_node(42613).await;
    let mut b = start_node(42613).await;
    establish_pair(&mut a, &mut b).await;

    b.engine.set_sync_mode(SyncMode::Off);
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

/// Sync direction policy: a send-only node refuses inbound sync but still
/// broadcasts its own copies
#[tokio::test]
async fn send_only_mode_rejects_inbound_but_broadcasts() {
    let mut a = start_node(42631).await;
    let mut b = start_node(42631).await;
    establish_pair(&mut a, &mut b).await;

    b.engine.set_sync_mode(SyncMode::Send);
    // Direction 1: A → B is refused (B does not receive)
    inject_text(&a, "b should reject this", now_ms()).await;
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    let err = result.expect_err("仅发出的对端应拒收");
    assert!(err.contains("disabled"), "拒因应含 disabled: {err}");
    assert_no_apply(&mut b.events, Duration::from_millis(800)).await;

    // Direction 2: B → A still lands (B's send direction is on)
    inject_text(&b, "a should apply this", now_ms()).await;
    let applied = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. } => apply_text(content),
        _ => None,
    })
    .await;
    assert_eq!(applied, "a should apply this");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Sync direction policy: a receive-only node never broadcasts its own copies
/// but still accepts inbound sync
#[tokio::test]
async fn receive_only_mode_applies_inbound_but_stays_silent() {
    let mut a = start_node(42632).await;
    let mut b = start_node(42632).await;
    establish_pair(&mut a, &mut b).await;

    a.engine.set_sync_mode(SyncMode::Receive);
    // Direction 1: A's copy never goes out (no SyncSent, no Apply on the
    // peer; LocalCopied is still emitted)
    inject_text(&a, "must stay on a", now_ms()).await;
    assert_no_apply(&mut b.events, Duration::from_millis(800)).await;
    let mut saw_local_copied = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, a.events.recv()).await {
        match event {
            EngineEvent::SyncSent { .. } => panic!("仅接收模式不应发起同步"),
            EngineEvent::LocalCopied { .. } => saw_local_copied = true,
            _ => {}
        }
    }
    assert!(saw_local_copied, "仅接收只停广播, 本机历史事件仍应上报");

    // Direction 2: B → A still lands
    inject_text(&b, "delivered to a", now_ms()).await;
    let applied = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. } => apply_text(content),
        _ => None,
    })
    .await;
    assert_eq!(applied, "delivered to a");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Sender-side pause: once sync is paused locally a copy must not go out (the
/// peer receives nothing and no SyncSent is produced)
///
/// The dual of the previous test, which only covers the receiving side.
#[tokio::test]
async fn disabled_sender_does_not_broadcast() {
    let mut a = start_node(42622).await;
    let mut b = start_node(42622).await;
    establish_pair(&mut a, &mut b).await;

    a.engine.set_sync_mode(SyncMode::Off);
    inject_text(&a, "must stay local", now_ms()).await;

    // The peer must not apply it
    assert_no_apply(&mut b.events, Duration::from_millis(800)).await;
    // No outbound attempt locally either (LocalCopied still fires: history
    // records as usual)
    let mut saw_local_copied = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, a.events.recv()).await {
        match event {
            EngineEvent::SyncSent { .. } => panic!("熔断状态下不应发起同步"),
            EngineEvent::LocalCopied { .. } => saw_local_copied = true,
            _ => {}
        }
    }
    assert!(
        saw_local_copied,
        "熔断只停同步, 本机复制事件仍应上报(历史需要)"
    );

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Echo suppression: once a remote write loops back through the local
/// clipboard it must not be broadcast again (no loop forms)
#[tokio::test]
async fn echo_is_suppressed() {
    let mut a = start_node(42614).await;
    let mut b = start_node(42614).await;
    establish_pair(&mut a, &mut b).await;

    inject_text(&a, "echo-test", now_ms()).await;
    let applied = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. } => apply_text(content),
        _ => None,
    })
    .await;

    // Simulate B's shell layer writing the system clipboard, after which the
    // watcher spots the change and reports it
    inject_text(&b, &applied, now_ms()).await;

    // B's engine must swallow the echo: A must not receive an ApplyRemote
    // "from B"
    assert_no_apply(&mut a.events, Duration::from_millis(1200)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// LWW tie-break: when the remote timestamp is no newer than the latest local
/// copy, accept the frame but leave the clipboard alone
#[tokio::test]
async fn lww_ignores_stale_remote() {
    let mut a = start_node(42615).await;
    let mut b = start_node(42615).await;
    establish_pair(&mut a, &mut b).await;

    // B copies locally first (the time baseline); A receives that broadcast
    // normally and it is consumed here
    let base = now_ms();
    inject_text(&b, "b-fresh", base).await;
    wait_event(&mut a.events, |ev| {
        matches!(ev, EngineEvent::ApplyRemote { .. }).then_some(())
    })
    .await;

    // A injects an "older" copy (clock skew / late arrival): B must Ack but
    // not apply it
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

/// Unpairing: removed locally at once, and removed on the peer through the
/// Unpair notification
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

/// Pairings persist: restarting the engine against the same data directory
/// keeps the pairing
#[tokio::test]
async fn paired_survives_restart() {
    let mut a = start_node(42617).await;
    let mut b = start_node(42617).await;
    establish_pair(&mut a, &mut b).await;

    let a_dir = a._dir.0.clone();
    let b_fp = b.fingerprint.clone();
    a.engine.shutdown().await;
    // The write is a fire-and-forget blocking task; give it a beat to finish
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (clip_tx, clip_rx) = mpsc::channel(16);
    drop(clip_tx);
    let (engine, _events) = SyncEngine::start(
        EngineConfig {
            data_dir: a_dir,
            tcp_port: 0,
            discovery_port: 42617,
            passive: true,
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
    .await
    .expect("重启引擎失败");
    assert!(
        engine.paired_list().iter().any(|p| p.fingerprint == b_fp),
        "重启后配对关系应存续"
    );
    engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Unpaired sources are refused (the first security gate): after a one-sided
/// unpair, sync from the peer — which still believes it is paired — is
/// rejected with a structured reason and produces no ApplyRemote
#[tokio::test]
async fn unpaired_source_is_rejected() {
    let mut a = start_node(42618).await;
    let mut b = start_node(42618).await;
    establish_pair(&mut a, &mut b).await;

    // Remove on B only, skipping unpair's peer notification: a one-sided
    // unpair where A still believes it is paired
    b.engine.inner.remove_paired(&a.fingerprint).await;
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::Unpaired { .. }).then_some(())
    })
    .await;

    inject_text(&a, "should be rejected by gate", now_ms()).await;
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    let err = result.expect_err("未配对来源的同步应被拒绝");
    assert!(err.contains("not_paired"), "拒因应含 not_paired: {err}");
    assert_no_apply(&mut b.events, Duration::from_millis(800)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Concurrent pair requests: the later one supersedes the earlier, and the
/// earlier one's exit cleanup must not delete the later one's decision handle
/// — clicking "accept" has to make the surviving request succeed within
/// seconds
#[tokio::test]
async fn concurrent_pair_requests_resolve() {
    let a = start_node(42619).await;
    let mut b = start_node(42619).await;
    wait_discover(&a.engine, &b.fingerprint).await;
    wait_discover(&b.engine, &a.fingerprint).await;

    let spawn_pair = |engine: Arc<SyncEngine>, target: String| {
        tokio::spawn(async move { engine.pair(&target).await })
    };
    // The two requests enter the table in strict order (ordered by the
    // PairRequested event) to set up the supersede
    let first = spawn_pair(Arc::clone(&a.engine), b.fingerprint.clone());
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::PairRequested { .. }).then_some(())
    })
    .await;
    let second = spawn_pair(Arc::clone(&a.engine), b.fingerprint.clone());
    // Do **not** wait for a second PairRequested here: while a pending
    // pairing request from the same peer exists the engine does not notify
    // again (otherwise a peer that keeps reconnecting would flood the system
    // notifications). The supersede still happens, it just emits no event —
    // so wait out a time window for it to enter the table, which also leaves
    // the earlier request time to run "superseded → exit cleanup"
    tokio::time::sleep(Duration::from_millis(500)).await;
    b.engine.respond_pair(&a.fingerprint, true);

    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        (
            first.await.expect("配对任务崩溃"),
            second.await.expect("配对任务崩溃"),
        )
    })
    .await
    .expect("并发配对的两个事务都必须在秒级内有确定结局(不得挂满 300s)");
    // Supersede semantics: the earlier request is treated as rejected, the
    // later one (holding the live handle) succeeds
    assert!(first.is_err(), "被顶替的先到请求应按拒绝处理: {first:?}");
    second.expect("后到请求应配对成功");
    assert!(
        a.engine
            .paired_list()
            .iter()
            .any(|p| p.fingerprint == b.fingerprint)
    );

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Orphan echo regression: several remotes sync the same text one after the
/// other and only one echo is registered; once the loop-back has consumed it,
/// a user copying something else and then copying that same text again must
/// still broadcast, not be swallowed by a leftover registration
#[tokio::test]
async fn duplicate_echo_does_not_swallow_real_copy() {
    let mut a = start_node(42620).await;
    let mut b = start_node(42620).await;
    establish_pair(&mut a, &mut b).await;

    // A broadcasts the same text twice (in the field: several devices syncing
    // the same content to B in turn)
    let base = now_ms();
    inject_text(&a, "dup", base).await;
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::ApplyRemote { .. }).then_some(())
    })
    .await;
    inject_text(&a, "dup", base + 10).await;
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::ApplyRemote { .. }).then_some(())
    })
    .await;

    // The shell layer writes the clipboard and the watcher produces only one
    // loop-back (the second write of identical content is deduped by
    // content), consuming a single echo; the registration left over is the
    // orphan
    inject_text(&b, "dup", base + 20).await;
    // The user copies something else (the watcher's dedup baseline moves on)
    inject_text(&b, "other", base + 30).await;
    wait_event(&mut a.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. }
            if apply_text(content).as_deref() == Some("other") =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    // The user copies that same text again: it must broadcast and A must get it
    inject_text(&b, "dup", base + 40).await;
    let applied = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. }
            if apply_text(content).as_deref() == Some("dup") =>
        {
            apply_text(content)
        }
        _ => None,
    })
    .await;
    assert_eq!(applied, "dup");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Echo table semantics (pure data structure): one-shot consumption and
/// eviction at capacity
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

/// An echo registration nobody consumes must expire, or it swallows a real
/// local copy
///
/// What separates this from [`duplicate_echo_does_not_swallow_real_copy`] is
/// that **the loop-back event never happens at all**: when the text the shell
/// layer writes is byte-for-byte what the clipboard already holds, the
/// watcher's content dedup (same `last_hash` means skip) makes that write
/// produce no event, so the registration is neither consumed nor undone by a
/// failure branch — it is an orphan. Without expiry it sticks around and
/// swallows the user's next real copy of that same text whole: no broadcast,
/// no history entry, and no LWW baseline bump, so an older item on the peer
/// can then overwrite it.
#[tokio::test]
async fn orphan_echo_expires_instead_of_swallowing_real_copy() {
    let mut a = start_node(42621).await;
    let mut b = start_node(42621).await;
    establish_pair(&mut a, &mut b).await;

    let base = now_ms();
    // A syncs a piece of text to B; B accepts it and registers the echo
    inject_text(&a, "same", base).await;
    wait_event(&mut b.events, |ev| {
        matches!(ev, EngineEvent::ApplyRemote { .. }).then_some(())
    })
    .await;
    // Deliberately do **not** inject B's loop-back event: this reproduces
    // exactly the case where the written content equals what the clipboard
    // already holds and the watcher emits nothing because of dedup — the
    // registration becomes an orphan right there

    // Wait for the orphan to expire. Both event channels must be drained
    // throughout the window: engine events are send().await backpressure (the
    // consumption contract in config.rs), and the more nodes the concurrent
    // tests run the denser the PeerUp stream gets. Once the backlog in the
    // silent window passes the capacity of 64 the engine pump wedges on emit
    // — copies injected afterwards are never broadcast, which surfaces as
    // this test timing out for no visible reason while the real cause is the
    // total node count across the suite (two more two-node tests are enough
    // to trigger it)
    let drain_deadline = tokio::time::Instant::now() + ECHO_TTL + Duration::from_millis(300);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(drain_deadline) => break,
            _ = a.events.recv() => {}
            _ = b.events.recv() => {}
        }
    }

    // The user really copies that same text on B: it must broadcast as usual
    // and A must receive it
    inject_text(&b, "same", base + 1000).await;
    wait_event(&mut a.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. }
            if apply_text(content).as_deref() == Some("same") =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
}

// ---- blob sync (protocol 1.1: images / files) ----

/// Inject a copy event carrying arbitrary content
async fn inject_content(node: &TestNode, content: ClipboardContent, timestamp_ms: u64) {
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

/// Test image (3×2 RGBA, deliberately non-trivial values)
fn sample_image() -> ClipboardContent {
    let rgba: Vec<u8> = (0..3 * 2 * 4).map(|i| (i * 37 % 251) as u8).collect();
    ClipboardContent::Image {
        width: 3,
        height: 2,
        rgba,
    }
}

/// Images end to end: A copies an image → it lands on B's clipboard with the
/// RGBA byte-for-byte identical, plus echo suppression (B's loop-back is not
/// broadcast back to A)
#[tokio::test]
async fn image_syncs_end_to_end_with_echo_suppression() {
    let mut a = start_node(42633).await;
    let mut b = start_node(42633).await;
    establish_pair(&mut a, &mut b).await;

    let sent = sample_image();
    let sent_hash = sent.hash();
    inject_content(&a, sent.clone(), now_ms()).await;
    let (applied, hash) = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, hash, .. } => Some((content.clone(), hash.clone())),
        _ => None,
    })
    .await;
    // RGBA baseline: the decoded content matches what was sent byte for byte,
    // and the echo hash is computed over that same baseline
    let ClipboardContent::Image {
        width,
        height,
        rgba,
    } = applied
    else {
        panic!("应收到图像内容");
    };
    assert_eq!((width, height), (3, 2));
    assert_eq!(
        ClipboardContent::Image {
            width,
            height,
            rgba
        }
        .hash(),
        sent_hash
    );
    assert_eq!(hash, sent_hash);

    // Simulate the loop-back through B's watcher: the engine must swallow it
    // and not broadcast (A must not receive an ApplyRemote)
    inject_content(&b, sent, now_ms()).await;
    assert_no_apply(&mut a.events, Duration::from_millis(800)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Files end to end: contents land byte for byte in the receiver's
/// sync-files, hostile file names are sanitized, and duplicate names within
/// one batch get a counter suffix
#[tokio::test]
async fn files_sync_lands_sanitized() {
    let mut a = start_node(42634).await;
    let mut b = start_node(42634).await;
    establish_pair(&mut a, &mut b).await;

    // Source files: one plain name plus a hostile one (the sender already
    // reduces source names to the basename, so feed in two files that share a
    // name but differ in content to exercise the receiver's counter suffix)
    let src_dir = TempDir::new();
    std::fs::create_dir_all(&src_dir.0).unwrap();
    let f1 = src_dir.0.join("report.txt");
    std::fs::write(&f1, b"hello lanecho").unwrap();
    let sub = src_dir.0.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let f2 = sub.join("report.txt");
    std::fs::write(&f2, vec![0xAB; 3 * 1024 * 1024]).unwrap();

    inject_content(&a, ClipboardContent::Files(vec![f1, f2.clone()]), now_ms()).await;
    let applied = wait_event(&mut b.events, |ev| match ev {
        EngineEvent::ApplyRemote { content, .. } => Some(content.clone()),
        _ => None,
    })
    .await;
    let ClipboardContent::Files(paths) = applied else {
        panic!("应收到文件内容");
    };
    assert_eq!(paths.len(), 2);
    // Everything lands under sync-files in the receiver's data directory
    for p in &paths {
        assert!(
            p.starts_with(&b._dir.0),
            "应落在 B 的数据目录: {}",
            p.display()
        );
        assert!(
            p.to_string_lossy().contains("sync-files"),
            "应在 sync-files 下: {}",
            p.display()
        );
    }
    // Contents byte for byte; duplicate names in the batch get a counter
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"hello lanecho");
    assert_eq!(
        std::fs::read(&paths[1]).unwrap(),
        vec![0xAB; 3 * 1024 * 1024]
    );
    assert_eq!(paths[0].file_name().unwrap(), "report.txt");
    assert_eq!(paths[1].file_name().unwrap(), "report (1).txt");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Type toggles (receiver side): with images off the offer is rejected with
/// unsupported_type — which doubles as the 1.0 compatibility path, since on
/// the wire the response to an offer has the same shape byte for byte as the
/// older version, and the dialer must receive the reason gracefully instead
/// of dropping the connection and retrying
#[tokio::test]
async fn image_offer_rejected_when_type_disabled() {
    let mut a = start_node(42635).await;
    let mut b = start_node(42635).await;
    establish_pair(&mut a, &mut b).await;

    b.engine.set_sync_types(SyncTypes {
        text: true,
        images: false,
        files: true,
    });
    inject_content(&a, sample_image(), now_ms()).await;
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    let err = result.expect_err("类型关闭应拒绝");
    assert!(
        err.contains("unsupported_type"),
        "拒因应为 unsupported_type: {err}"
    );
    assert_no_apply(&mut b.events, Duration::from_millis(500)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// Size cap (receiver side, before the stream): when the total file size
/// exceeds B's cap the offer stage rejects with too_large, and nothing is
/// left behind in B's sync-files
#[tokio::test]
async fn oversized_files_rejected_before_stream() {
    let mut a = start_node(42636).await;
    let mut b = start_node(42636).await;
    establish_pair(&mut a, &mut b).await;

    b.engine.set_max_sync_file_bytes(1024); // B accepts 1KB only
    let src_dir = TempDir::new();
    std::fs::create_dir_all(&src_dir.0).unwrap();
    let big = src_dir.0.join("big.bin");
    std::fs::write(&big, vec![0u8; 64 * 1024]).unwrap();

    inject_content(&a, ClipboardContent::Files(vec![big]), now_ms()).await;
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    let err = result.expect_err("超限应拒绝");
    assert!(err.contains("too_large"), "拒因应为 too_large: {err}");
    // Rejected before the stream: the receiver must write nothing to disk
    let sync_files = b._dir.0.join("sync-files");
    let leftovers = std::fs::read_dir(&sync_files)
        .map(|it| it.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "offer 阶段拒绝不该有落盘");

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// LWW checked up front (blob): when B's local copy is newer, A's image offer
/// gets an Ack and no transfer follows (the rule that a peer need not tell
/// "applied" from "ignored" carries over to blobs)
#[tokio::test]
async fn stale_blob_offer_acked_without_transfer() {
    let mut a = start_node(42637).await;
    let mut b = start_node(42637).await;
    establish_pair(&mut a, &mut b).await;

    let base = now_ms();
    // B "copies" first to push the local LWW baseline forward
    inject_text(&b, "newer local", base + 5000).await;
    // Consume the normal B→A sync that follows
    let _ = wait_event(&mut a.events, |ev| {
        matches!(ev, EngineEvent::ApplyRemote { .. }).then_some(())
    })
    .await;

    // A sends the image with an earlier timestamp: B judges it stale, Acks
    // and does not receive it (SyncSent must be Ok)
    inject_content(&a, sample_image(), base + 1000).await;
    let result = wait_event(&mut a.events, |ev| match ev {
        EngineEvent::SyncSent { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await;
    assert!(result.is_ok(), "LWW 判旧应回 Ack 而非拒绝: {result:?}");
    assert_no_apply(&mut b.events, Duration::from_millis(500)).await;

    a.engine.shutdown().await;
    b.engine.shutdown().await;
}

/// **Regression guard**: a refusal during the handshake must reach the caller
/// as that refusal, not as a mystery.
///
/// The accept side can turn a connection away at the Hello gate — the
/// identity declared in Hello not matching the TLS certificate, an
/// incompatible version. It used to just close, leaving the dialer with a
/// bare "early eof" and no idea why; diagnosing one such case (a resumed TLS
/// session that carried no client certificate, so the native client's
/// verification callback never ran) took a long time for exactly this reason.
#[tokio::test]
async fn handshake_surfaces_a_refusal_from_the_peer() {
    let dir = TempDir::new();
    let identity = crate::identity::DeviceIdentity::load_or_create(&dir.0).unwrap();
    let (mut client, mut server) = tokio::io::duplex(4096);

    // Stand-in accept side: read the Hello, refuse instead of answering
    let peer = tokio::spawn(async move {
        let _ = crate::protocol::read_frame(&mut server).await;
        let _ = crate::protocol::write_frame(
            &mut server,
            &ControlMessage::SyncRejected {
                reason_code: crate::protocol::reason_code::IDENTITY_MISMATCH.to_string(),
            },
        )
        .await;
    });

    let err = net::handshake_out(&mut client, &identity, "whatever")
        .await
        .expect_err("a refusal must not be reported as success");
    peer.await.unwrap();

    match err {
        SyncError::Rejected(code) => {
            assert_eq!(code, crate::protocol::reason_code::IDENTITY_MISMATCH)
        }
        other => panic!("expected the peer's reason, got: {other}"),
    }
}
