// Typed wrappers around the Tauri commands

import { invoke } from "@tauri-apps/api/core";
import type {
  AutoPasteStatus,
  DeviceDto,
  EntryTextDto,
  HistoryEntryDto,
  IgnoredApp,
  PeerDto,
  SelfInfoDto,
  Settings,
} from "./types";

export const api = {
  /** Local device info */
  getSelfInfo: () => invoke<SelfInfoDto>("get_self_info"),
  /** Device list (online ∪ paired) */
  listDevices: () => invoke<DeviceDto[]>("list_devices"),
  /** Settings read and write */
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  /** Hot-update the local display name (null / empty string = follow the
   *  hostname again) */
  setDisplayName: (name: string | null) =>
    invoke<void>("set_display_name", { name: name ?? null }),
  /** Start pairing with a device (blocks until the peer confirms or it times
   *  out) */
  pairDevice: (fingerprint: string) => invoke<void>("pair_device", { fingerprint }),
  /** Answer an inbound pairing request (the counterpart of the
   *  pair-requested event) */
  respondPair: (fingerprint: string, accept: boolean) =>
    invoke<void>("respond_pair", { fingerprint, accept }),
  /** Snapshot of pending pairing requests (pulled on mount to recover events
   *  lost during the startup window) */
  listPendingPairs: () => invoke<PeerDto[]>("list_pending_pairs"),
  /** Unpair */
  unpairDevice: (fingerprint: string) => invoke<void>("unpair_device", { fingerprint }),
  /** History: the list (a light projection without full text; the backend
   *  reads the sort order from settings and pinned entries always lead) */
  listHistory: () => invoke<HistoryEntryDto[]>("list_history"),
  /** History: full-text search (matched Rust-side, returns the matching entry
   *  IDs; the frontend filters in list order) */
  searchHistory: (query: string) => invoke<string[]>("search_history", { query }),
  /** History: the detail text of a text entry by ID (already truncated to the
   *  render limit on the Rust side) */
  historyEntryText: (id: string) => invoke<EntryTextDto>("history_entry_text", { id }),
  /** History: restore the chosen entry to the clipboard (counts as a user
   *  copy, so it broadcasts as usual) */
  copyHistoryEntry: (id: string) => invoke<void>("copy_history_entry", { id }),
  /** Hide the history panel (funnelled through the Rust side, which on macOS
   *  also hands focus back to the previous application) */
  hidePanel: () => invoke<void>("hide_panel"),
  /** History: delete one / clear all / pin */
  deleteHistoryEntry: (id: string) => invoke<void>("delete_history_entry", { id }),
  clearHistory: () => invoke<void>("clear_history"),
  pinHistoryEntry: (id: string, pinned: boolean) =>
    invoke<void>("pin_history_entry", { id, pinned }),
  /** History: raw PNG bytes of an image entry (rendered by the preview card;
   *  a binary IPC avoids base64) */
  historyImagePng: (id: string) => invoke<ArrayBuffer>("history_image_png", { id }),
  /** Source application icon as PNG (rejects when nothing is cached; callers
   *  just hide the icon) */
  appIconPng: (name: string) => invoke<ArrayBuffer>("app_icon_png", { name }),
  /** Preview card: show (called by the card itself once it has measured;
   *  height = the actual content height, and the window follows it) */
  showPreview: (anchorY: number | null, height: number) =>
    invoke<void>("show_preview", { anchorY, height }),
  /** Preview card: hide (the highlighted row went away or the search matched
   *  nothing) */
  hidePreview: () => invoke<void>("hide_preview"),
  /** Open the settings window (the preferences item in the panel's footer
   *  menu) */
  showSettingsWindow: () => invoke<void>("show_settings_window"),
  /** Open the settings window on the about tab (panel footer menu / Linux
   *  tray menu) */
  showAbout: () => invoke<void>("show_about"),
  /** Application version (shown on the about page) */
  appVersion: () => invoke<string>("app_version"),
  /** Quit the app (panel footer menu; takes the normal exit path so the
   *  engine shuts down gracefully) */
  quitApp: () => invoke<void>("quit_app"),
  /** Fit the settings window height to the current tab's content (called
   *  once the frontend has measured; the width is left alone) */
  resizeSettingsWindow: (contentHeight: number) =>
    invoke<void>("resize_settings_window", { contentHeight }),
  /** History: bytes used on disk */
  historyUsage: () => invoke<number>("history_usage"),
  /** Incognito mode (pauses history recording for this session) */
  setIncognito: (on: boolean) => invoke<void>("set_incognito", { on }),
  getIncognito: () => invoke<boolean>("get_incognito"),
  /** Whether panel vibrancy is active (when true the frontend switches to the
   *  translucent background variables) */
  windowEffectsActive: () => invoke<boolean>("window_effects_active"),
  /** Slot hotkeys that failed to register (the N of Alt+N; the settings page
   *  reports them as taken) */
  getSlotHotkeyFailures: () => invoke<number[]>("get_slot_hotkey_failures"),
  /** Pick an application through the system dialog and resolve it into an
   *  ignore entry (null when the user cancels) */
  pickIgnoredApp: () => invoke<IgnoredApp | null>("pick_ignored_app"),
  /** Whether auto-paste can work on this machine */
  autoPasteStatus: () => invoke<AutoPasteStatus>("auto_paste_status"),
  /** Ask for the permission auto-paste needs (macOS raises the system
   *  authorization prompt) and read the state back */
  requestAutoPastePermission: () => invoke<AutoPasteStatus>("request_auto_paste_permission"),
};
