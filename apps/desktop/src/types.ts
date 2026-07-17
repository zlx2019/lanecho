// 前后端共享 DTO(镜像 src-tauri 的 Serialize 结构, 键名 camelCase)

/** 用户设置(镜像 settings.rs 的 Settings; 昵称不在此 —— identity.json 为唯一真源) */
export interface Settings {
  /** TCP 监听端口(0 = 随机; 重启后生效) */
  tcpPort: number;
  /** 开机自启 */
  autostart: boolean;
  /** 同步开关(熔断闸) */
  syncEnabled: boolean;
  /** 剪贴板被远端覆盖时弹系统通知 */
  notifyOnSync: boolean;
  /** 界面语言: "zh" / "en"; 空 = 未初始化 */
  language: string;
  /** 历史保留条目上限 */
  historyMaxEntries: number;
  /** 历史记录类型开关 */
  historyRecordText: boolean;
  historyRecordImages: boolean;
  historyRecordFiles: boolean;
  /** 历史排序: "recent" / "frequent" */
  historySort: string;
  /** 历史面板唤起快捷键(空串 = 禁用) */
  panelHotkey: string;
  /** 序号槽位直贴(Alt+1..6)开关 */
  slotHotkeys: boolean;
}

/** 历史条目(镜像 history.rs 的 HistoryEntry) */
export interface HistoryEntryDto {
  id: string;
  /** "text" | "image" | "files" */
  kind: string;
  text?: string;
  blobHash?: string;
  files?: string[];
  preview: string;
  contentHash: string;
  firstCopiedAt: number;
  lastCopiedAt: number;
  copyCount: number;
  origin?: string;
  pinned: boolean;
}

/** 本机身份信息 */
export interface SelfInfoDto {
  name: string;
  deviceId: string;
  fingerprint: string;
  platform: string;
  port: number;
}

/** 节点信息(peer-up / pair-requested / paired 事件载荷) */
export interface PeerDto {
  deviceId: string;
  name: string;
  fingerprint: string;
  platform: string;
  osVersion: string | null;
}

/** 设备列表条目: 在线节点与已配对(可能离线)设备的合并视图 */
export interface DeviceDto {
  name: string;
  fingerprint: string;
  platform: string | null;
  osVersion: string | null;
  online: boolean;
  paired: boolean;
}

/** 远端同步事件(clipboard-synced 载荷) */
export interface SyncedDto {
  fromName: string;
  preview: string;
  at: number;
}
