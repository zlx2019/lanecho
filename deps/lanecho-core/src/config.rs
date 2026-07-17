//! 引擎调优常量: 心跳/超时/轮询等散落参数的集中定义(deskmate 约定继承)
//!
//! 一处可查、一处可调; 若未来需要运行时配置(设置页/CLI 参数),
//! 以本模块为字段清单升级为注入式配置结构, 常量转为其默认值。
//!
//! 端口与协议侧上限不在此列: 端口默认值(`DEFAULT_TCP_PORT` 等)在 crate 根,
//! 帧大小上限是双端一致的协议合同, 定义在 [`crate::protocol`]。

use std::time::Duration;

// ---- 发现层(discovery, 数值全部继承 deskmate 实战结论)----

/// 心跳间隔: UDP 组播 announce 的周期
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// 节点超时: 超过该时长未见心跳即判定下线(容忍连续丢 2 次心跳)
pub const PEER_TIMEOUT: Duration = Duration::from_secs(15);

/// 崩溃节点探活间隔: "仅 mDNS 在线且 UDP 静默"的节点每隔该时长 TCP 探测一次
/// (取值边界的完整论证见 deskmate config.rs, 此处沿用结论)
pub const PEER_PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// 单次探活的 TCP 连接超时
pub const PEER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 事件通道容量。两处复用但溢出语义不同:
/// - discovery 的节点事件: `try_send` 满时丢弃, 消费方可用快照兜底;
/// - sync 引擎事件: `send().await` 背压阻塞, 消费端(桌面事件泵)停滞时
///   发送侧会整体挂住 —— 消费端必须保持快速消费, 不得做慢操作
pub const EVENT_CHANNEL_CAP: usize = 64;

// ---- 剪贴板监视(watcher, 决策 #4)----

/// 变化戳轮询周期(macOS changeCount / Windows sequence number, 均为
/// 单次系统调用级的廉价读取): 250ms 把"复制→对端可粘贴"的最坏检测
/// 延迟砍到 1/4 秒, 翻倍的调用频率在微秒级成本下可忽略
pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// Linux 退化路径的轮询周期(无廉价变化戳, 读文本比对代价更高, 放宽)
pub const WATCH_INTERVAL_FALLBACK: Duration = Duration::from_secs(1);

// ---- 同步引擎(sync)----

/// 跨设备同步的文本载荷上限(512 KiB): 帧上限 1MiB 之内为 JSON 转义
/// 与元数据留足余量; 超限不广播(记日志), 本机历史不受此限(决策 #9)
pub const MAX_SYNC_TEXT_BYTES: usize = 512 * 1024;

/// 握手/应答类消息的等待时长
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// 等待对端配对决策的超时时长(人在环上, 用长超时)
pub const PAIR_DECISION_TIMEOUT: Duration = Duration::from_secs(300);

/// 单个候选地址的 TCP 连接超时(多网卡逐个尝试, 不宜过长)
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 最近远端写入的回声登记容量(内容哈希环; 正常只需 1-2 条,
/// 余量应对"连续多条同步接连到达而轮询尚未追上"的堆叠)
pub const ECHO_RECENT_CAP: usize = 8;

// ---- 接收端连接治理(receiver, 模式继承 deskmate)----

/// 并发连接数上限: 超出直接拒绝新连接(防 slow-loris 耗尽 fd/内存)
///
/// lanecho 连接是"拨号-单帧-即走"(决策 #3), 无长驻数据流, 64 足够宽松。
pub const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// 未认证阶段(TLS 握手 + 首帧)的超时, 挡住"连上后不说话"的占坑连接
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// 已认证连接的帧间隙超时: 事务毫秒级完成, 静默挂起的半开连接
/// (对端睡眠/拔线)不得长期占用连接配额 —— 64 条半开即可瘫痪全部入站
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
