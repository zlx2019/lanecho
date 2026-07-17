/** Tauri 事件名: 与 src-tauri/src/bridge.rs 的 events 模块一一对应, 改名必须两端同步 */
export const EVENTS = {
  /** 节点上线/信息更新(载荷 PeerDto) */
  PEER_UP: "peer-up",
  /** 节点下线(载荷为指纹字符串) */
  PEER_DOWN: "peer-down",
  /** 收到配对请求, 等待用户决策(载荷 PeerDto) */
  PAIR_REQUESTED: "pair-requested",
  /** 配对成立(载荷 PeerDto) */
  PAIRED: "paired",
  /** 配对解除(载荷为指纹字符串) */
  UNPAIRED: "unpaired",
  /** 远端剪贴板已应用到本机(载荷 SyncedDto) */
  CLIPBOARD_SYNCED: "clipboard-synced",
  /** 同步开关变化 —— 托盘切换回显设置窗(载荷 boolean) */
  SYNC_STATE: "sync-state-changed",
} as const;
