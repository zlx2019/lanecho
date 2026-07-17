// Tauri commands 的类型化封装

import { invoke } from "@tauri-apps/api/core";
import type { DeviceDto, HistoryEntryDto, PeerDto, SelfInfoDto, Settings } from "./types";

export const api = {
  /** 本机信息 */
  getSelfInfo: () => invoke<SelfInfoDto>("get_self_info"),
  /** 设备列表(在线 ∪ 已配对) */
  listDevices: () => invoke<DeviceDto[]>("list_devices"),
  /** 设置读写 */
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  /** 热更新本机展示名(null/空串 = 恢复跟随 hostname) */
  setDisplayName: (name: string | null) =>
    invoke<void>("set_display_name", { name: name ?? null }),
  /** 向指定设备发起配对(阻塞至对端确认或超时) */
  pairDevice: (fingerprint: string) => invoke<void>("pair_device", { fingerprint }),
  /** 回应入站配对请求(对应 pair-requested 事件) */
  respondPair: (fingerprint: string, accept: boolean) =>
    invoke<void>("respond_pair", { fingerprint, accept }),
  /** 待决配对请求快照(挂载补拉, 补启动窗口期丢失的事件) */
  listPendingPairs: () => invoke<PeerDto[]>("list_pending_pairs"),
  /** 解除配对 */
  unpairDevice: (fingerprint: string) => invoke<void>("unpair_device", { fingerprint }),
  /** 历史: 列表(排序方式后端读设置, pinned 恒顶) */
  listHistory: () => invoke<HistoryEntryDto[]>("list_history"),
  /** 历史: 选中条目还原写入剪贴板(视同用户复制, 会正常广播) */
  copyHistoryEntry: (id: string) => invoke<void>("copy_history_entry", { id }),
  /** 收起历史面板(Rust 侧统一收口: macOS 顺带归还焦点给前一应用) */
  hidePanel: () => invoke<void>("hide_panel"),
  /** 历史: 删除单条 / 清空 / 固定 */
  deleteHistoryEntry: (id: string) => invoke<void>("delete_history_entry", { id }),
  clearHistory: () => invoke<void>("clear_history"),
  pinHistoryEntry: (id: string, pinned: boolean) =>
    invoke<void>("pin_history_entry", { id, pinned }),
  /** 历史: 磁盘占用字节数 */
  historyUsage: () => invoke<number>("history_usage"),
  /** 无痕模式(暂停历史记录, 会话级) */
  setIncognito: (on: boolean) => invoke<void>("set_incognito", { on }),
  getIncognito: () => invoke<boolean>("get_incognito"),
};
