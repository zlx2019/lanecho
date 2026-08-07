// DTOs shared by frontend and backend (mirroring the Serialize structs in
// src-tauri, with camelCase keys)

/** User settings (mirrors Settings in settings.rs; the display name is not
 *  here — identity.json is its only source of truth) */
export interface Settings {
  /** TCP listen port (0 = random; takes effect after a restart) */
  tcpPort: number;
  /** Launch at login */
  autostart: boolean;
  /** Sync switch (a coarse redundant copy, always equal to
   *  syncMode != "off"; the backend double-writes both on save) */
  syncEnabled: boolean;
  /** Sync direction policy: "off" / "both" / "send" / "receive" */
  syncMode: string;
  /** Type toggles for syncing (they gate both sending and receiving) */
  syncText: boolean;
  syncImages: boolean;
  syncFiles: boolean;
  /** Total size cap for file sync (MB, 1-512) */
  maxSyncFileMb: number;
  /** Raise a system notification when a remote overwrites the clipboard */
  notifyOnSync: boolean;
  /** Interface language: "zh" / "en"; empty = not initialized */
  language: string;
  /** Cap on retained history entries */
  historyMaxEntries: number;
  /** Type toggles for history recording */
  historyRecordText: boolean;
  historyRecordImages: boolean;
  historyRecordFiles: boolean;
  /** History sort order: "recent" / "frequent" */
  historySort: string;
  /** Hotkey that opens the history panel (empty string = disabled) */
  panelHotkey: string;
  /** Toggle for direct paste from numbered slots (modifier+1..6) */
  slotHotkeys: boolean;
  /** Modifier for the slot shortcuts: "CmdOrCtrl" / "Alt" / "Ctrl" */
  slotModifier: string;
  /** Preview card delay (ms; 0 = immediate) */
  previewDelayMs: number;
  /** Paste automatically after an entry is chosen (macOS / Windows) */
  autoPaste: boolean;
}

/** Whether auto-paste can work here: `supported` is the platform, `permitted`
 *  the operating system permission (macOS Accessibility) */
export interface AutoPasteStatus {
  supported: boolean;
  permitted: boolean;
}

/** List projection of a history entry (mirrors HistoryEntryMeta in
 *  history.rs)
 *
 *  **No full text**: the list is re-pulled whole on every open, so carrying
 *  the full text (up to 5MB per entry) through IPC would be pure waste; the
 *  preview card fetches one entry at a time via historyEntryText, and search
 *  goes through searchHistory */
export interface HistoryEntryDto {
  id: string;
  /** "text" | "image" | "files" */
  kind: string;
  files?: string[];
  preview: string;
  firstCopiedAt: number;
  lastCopiedAt: number;
  copyCount: number;
  origin?: string;
  /** Source application (the frontmost app at the time of a local copy;
   *  remote entries have no such field) */
  sourceApp?: string;
  pinned: boolean;
  /** Text length in bytes (kind=text; 0 for anything else) */
  textLen: number;
}

/** Text payload for the preview card (returned by the history_entry_text
 *  command; the truncation happens on the Rust side) */
export interface EntryTextDto {
  /** The text, already truncated to the render limit */
  text: string;
  /** Total character count of the full text */
  totalChars: number;
  /** Whether it was truncated */
  truncated: boolean;
}

/** Panel → preview card push payload (the PREVIEW_ENTRY event, purely between
 *  frontend windows) */
export interface PreviewPayload {
  /** The highlighted entry (the list projection, without full text) */
  entry: HistoryEntryDto;
  /** Logical y of the highlighted row relative to the top of the panel
   *  document, so the card lines up with the row (null when unavailable) */
  anchorY: number | null;
  /** The panel's current language, so the card aligns directly instead of
   *  spending another getSettings IPC on every push */
  lang: string;
  /** Detail text for text entries (the panel fetched it before pushing; null
   *  on failure, in which case the card falls back to preview) */
  text: string | null;
  /** Whether the detail text was truncated (drives the truncation notice) */
  textTruncated: boolean;
}

/** Local device identity */
export interface SelfInfoDto {
  name: string;
  deviceId: string;
  fingerprint: string;
  platform: string;
  port: number;
}

/** Peer info (payload of the peer-up / pair-requested / paired events) */
export interface PeerDto {
  deviceId: string;
  name: string;
  fingerprint: string;
  platform: string;
  osVersion: string | null;
}

/** Device list entry: the merged view of online peers and paired devices,
 *  which may be offline */
export interface DeviceDto {
  name: string;
  fingerprint: string;
  platform: string | null;
  osVersion: string | null;
  online: boolean;
  paired: boolean;
}

/** Remote sync event (the clipboard-synced payload) */
export interface SyncedDto {
  fromName: string;
  preview: string;
  at: number;
}
