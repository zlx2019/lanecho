/** Tauri event names: one for one with the events module in
 *  src-tauri/src/bridge.rs — a rename has to happen on both sides */
export const EVENTS = {
  /** A peer came online or updated its info (payload: PeerDto) */
  PEER_UP: "peer-up",
  /** A peer went offline (payload: the fingerprint string) */
  PEER_DOWN: "peer-down",
  /** A pairing request arrived and awaits the user's decision (payload:
   *  PeerDto) */
  PAIR_REQUESTED: "pair-requested",
  /** Pairing established (payload: PeerDto) */
  PAIRED: "paired",
  /** Pairing removed (payload: the fingerprint string) */
  UNPAIRED: "unpaired",
  /** A remote clipboard was applied locally (payload: SyncedDto) */
  CLIPBOARD_SYNCED: "clipboard-synced",
  /** The sync switch changed — echoes a tray toggle back to the settings
   *  window (payload: boolean) */
  SYNC_STATE: "sync-state-changed",
  /** History content changed (added / counted / deleted / cleared), no
   *  payload */
  HISTORY_CHANGED: "history-changed",
  /** Incognito mode changed — echoes a tray toggle back (payload: boolean) */
  INCOGNITO_STATE: "incognito-changed",
  /** Panel → preview card push of the highlighted entry (payload:
   *  HistoryEntryDto); a frontend-only window-to-window event (emitTo) that
   *  never goes through bridge.rs and has no Rust-side counterpart */
  PREVIEW_ENTRY: "preview-entry",
  /** Panel → preview card page-scroll request (payload: 1 = down, -1 = up);
   *  frontend-only, like PREVIEW_ENTRY. PgUp/PgDn read a clipped card from
   *  the panel keyboard — on Windows the card's cursor pass-through can never
   *  be taken back (tao re-runs ShowWindow on any flag change while visible),
   *  so the wheel cannot reach it and the keyboard is the only way */
  PREVIEW_SCROLL: "preview-scroll",
} as const;
