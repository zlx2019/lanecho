// Tauri commands 的类型化封装

import { invoke } from "@tauri-apps/api/core";
import type { DeviceDto, SelfInfoDto, Settings } from "./types";

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
  /** 解除配对 */
  unpairDevice: (fingerprint: string) => invoke<void>("unpair_device", { fingerprint }),
};
