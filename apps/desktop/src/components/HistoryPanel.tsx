// Floating history panel: search + numbered slots + fully keyboard reachable
//
// Its own WebView window (label = "panel", borderless, rounded, hides on
// blur); main.tsx routes to this component by window label. Choosing an entry
// restores it to the clipboard and hides the panel; that write counts as a
// user copy, so the watcher broadcasts and counts it as usual.
// Maccy-style visuals: compact single-line rows + solid rounded highlight +
// slot badge on the right; resting on the highlighted row pops the preview
// card (PreviewCard) beside the panel.
//
// Performance rules (sweeping the mouse across the list must stay smooth):
// - Row components are memoized: moving the highlight re-renders only the
//   two rows involved, not the whole list
// - The list payload carries no full text: search runs Rust-side through
//   search_history, and preview text is fetched per entry right before the
//   push — 5MB-class text no longer rides the list through IPC
// - Incremental rendering: only a bit more than one screen of rows mounts
//   initially, growing on scroll / keyboard navigation (this guards the
//   worst case of a 10k entry cap; the default 200 is imperceptible)
// - Language rides the payload: the preview card no longer re-reads
//   getSettings on every push

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api";
import { EVENTS } from "../events";
import { formatError, useI18n, type Locale } from "../i18n";
import { useTheme } from "../theme";
import type { HistoryEntryDto, PreviewPayload } from "../types";
import { Button } from "./ModalShell";

/** Whether we are running inside the Tauri runtime */
const hasTauri = "__TAURI_INTERNALS__" in window;

/** Modifier shown on slot badges (Alt+N is what gets registered: macOS
 *  displays ⌥, every other platform Alt+) */
const SLOT_MOD = navigator.userAgent.includes("Mac") ? "⌥" : "Alt+";

/** Fallback preview-card delay (ms): used before settings load, kept in sync
 *  with the Settings default. The delay doubles as a coalescing window while
 *  sweeping rows: rows passed through are not pushed, only the row you stop
 *  on is — too long feels sluggish, too short makes the card flicker during a
 *  sweep; the user can change it on the General settings page */
const DEFAULT_PREVIEW_DELAY_MS = 150;

/** Search debounce (ms): one IPC per keystroke is too dense, so keystrokes
 *  inside the window coalesce into a single call */
const SEARCH_DEBOUNCE_MS = 80;

/** Initial row count and growth step for incremental rendering (a screen
 *  holds about 12 rows, so 120 covers several screens of scrolling) */
const RENDER_CHUNK_ROWS = 120;

/** Floating history panel */
export function HistoryPanel() {
  const { t, lang, setLang } = useI18n();
  // The panel is its own WebView document and applies the theme itself (the
  // localStorage key is shared)
  useTheme();
  // null = not loaded yet (render blank instead of flashing a fake "no
  // history" state)
  const [entries, setEntries] = useState<HistoryEntryDto[] | null>(null);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState("");
  // Preview-card delay (a setting; the reload on every panel open picks up
  // the latest value)
  const [previewDelay, setPreviewDelay] = useState(DEFAULT_PREVIEW_DELAY_MS);
  // Preview suppression: no card while the pointer rests off the rows (search
  // row / blank tail of the list / footer menu)
  const [previewSuppressed, setPreviewSuppressed] = useState(false);
  // Clear-history confirmation (an in-panel overlay, not a native dialog —
  // a native one steals focus and the panel hides itself)
  const [clearAsking, setClearAsking] = useState(false);
  // Set of matching entry IDs (null = no filter; full-text matching happens
  // Rust-side, see the search effect)
  const [matchIds, setMatchIds] = useState<Set<string> | null>(null);
  // Incremental render base (the scroll sentinel grows it by one step; reset
  // on every open)
  const [renderBase, setRenderBase] = useState(RENDER_CHUNK_ROWS);
  const inputRef = useRef<HTMLInputElement>(null);
  // Highlight source (keyboard / mouse) and the highlighted row element: only
  // keyboard navigation scrolls
  const inputSourceRef = useRef<"keyboard" | "mouse">("keyboard");
  const highlightElRef = useRef<HTMLDivElement | null>(null);
  // Entry list container (its horizontal midline is the row-band probe point,
  // see onPanelMouseMove)
  const listRef = useRef<HTMLDivElement>(null);
  // Scroll sentinel (at the list tail; entering the viewport grows the render
  // base)
  const sentinelRef = useRef<HTMLDivElement>(null);
  // Search request sequence (responses can still arrive out of order after
  // debouncing, so only the latest one is accepted)
  const searchSeqRef = useRef(0);
  // Preview push sequence (the on-demand text fetch is async; a late result
  // for an old highlight must not replace the current card)
  const previewSeqRef = useRef(0);

  const reload = useCallback(async () => {
    if (!hasTauri) return;
    try {
      // The backend reads the sort order from settings; the two IPC calls are
      // independent and run in parallel (halves the open latency)
      const [settings, list] = await Promise.all([api.getSettings(), api.listHistory()]);
      // Language follows settings: this panel is a long-lived hidden document
      // and is not rebuilt when the main window switches language, so align
      // on every refresh (React skips a same-value setState, so re-rendering
      // costs nothing)
      if (settings.language === "zh" || settings.language === "en") {
        setLang(settings.language);
      }
      setPreviewDelay(settings.previewDelayMs);
      setEntries(list);
    } catch (e) {
      console.error(e);
    }
  }, [setLang]);

  // Switch to a translucent background when vibrancy is active (the
  // [data-vibrancy] variables); platforms without vibrancy keep the opaque
  // variables so a transparent window never shows the desktop through
  // (document-level transparency is handled by main.tsx's [data-floating])
  useEffect(() => {
    if (!hasTauri) return;
    api
      .windowEffectsActive()
      .then((active) => {
        if (active) document.documentElement.dataset.vibrancy = "1";
      })
      .catch(console.error);
  }, []);

  useEffect(() => {
    if (!hasTauri) return;
    let alive = true;
    const unsubs: UnlistenFn[] = [];
    const add = (subscription: Promise<UnlistenFn>) => {
      subscription
        .then((unsub) => {
          if (alive) {
            unsubs.push(unsub);
          } else {
            unsub();
          }
        })
        .catch(console.error);
    };
    reload();
    // This panel is a long-lived document that stays hidden most of the
    // time: skip refreshes while hidden (re-pulling the whole list on every
    // copy is pure waste); the tauri://focus refresh on open catches up
    add(
      listen(EVENTS.HISTORY_CHANGED, () => {
        if (document.visibilityState === "visible") reload();
      }),
    );
    // Every open (window focus) resets state: refresh the list, clear the
    // search, focus the input
    add(
      getCurrentWindow().listen("tauri://focus", () => {
        reload();
        setQuery("");
        setMatchIds(null);
        setSelected(0);
        setError("");
        setRenderBase(RENDER_CHUNK_ROWS);
        // A fresh open starts **suppressed**. The panel is opened with the
        // pointer somewhere else entirely — on the tray icon, or wherever it
        // happened to rest when the hotkey was pressed — and the first row is
        // highlighted only to give the keyboard a starting point. Lifting the
        // suppression here made the card pop up beside the panel on its own,
        // with the pointer on no row at all, and vanish again as soon as the
        // mouse moved and the row-band probe ran. Moving onto a row lifts it
        // (onPanelMouseMove), and so does ↑/↓.
        setPreviewSuppressed(true);
        setClearAsking(false);
        inputRef.current?.focus();
      }),
    );
    return () => {
      alive = false;
      unsubs.forEach((unsub) => unsub());
    };
  }, [reload]);

  const loaded = useMemo(() => entries ?? [], [entries]);

  // Search: full text stays Rust-side, the frontend only sends the query and
  // gets back the set of matching IDs — no more pulling every entry's full
  // text over to build a frontend index. Debouncing coalesces consecutive
  // keystrokes; a change to entries (copy / delete) re-runs the current query
  // so the match set never goes stale
  useEffect(() => {
    if (!hasTauri || !query) {
      setMatchIds(null);
      return;
    }
    const seq = ++searchSeqRef.current;
    const timer = window.setTimeout(() => {
      api
        .searchHistory(query)
        .then((ids) => {
          if (searchSeqRef.current === seq) setMatchIds(new Set(ids));
        })
        .catch(console.error);
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query, entries]);

  const filtered = useMemo(() => {
    if (matchIds === null) return loaded;
    return loaded.filter((e) => matchIds.has(e.id));
  }, [loaded, matchIds]);
  // Clamp both ends: arrow keys on an empty list can drive selected to -1
  const highlight = Math.max(0, Math.min(selected, filtered.length - 1));
  // Incremental render window: the sentinel drives the base, but whatever row
  // keyboard navigation reaches must be rendered (deriving it inside the same
  // render guarantees the row is in the DOM for scrollIntoView and row-band
  // probing)
  const renderLimit = Math.max(renderBase, highlight + 20);
  const visibleRows = filtered.length > renderLimit ? filtered.slice(0, renderLimit) : filtered;

  // Scroll sentinel: grow the render base by one step as the list tail comes
  // close (rootMargin fires a screen early, so rows are in place before the
  // scroll reaches them and no loading gap shows)
  const hasMoreRows = filtered.length > renderLimit;
  useEffect(() => {
    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMoreRows) return;
    const observer = new IntersectionObserver(
      (hits) => {
        if (hits.some((h) => h.isIntersecting)) {
          setRenderBase((base) => base + RENDER_CHUNK_ROWS);
        }
      },
      { root: listRef.current, rootMargin: "300px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [hasMoreRows]);

  // Scroll the highlighted row into view — keyboard navigation only. If hover
  // scrolled too, scrollIntoView on an edge row would move the content under
  // the pointer, and that feeds back into the hover highlight as a loop; and
  // when keyboard scrolling slides a new row under the pointer, the mouse
  // would steal the highlight (mousemove is immune: scrolling the list
  // without moving the mouse produces no mousemove)
  useEffect(() => {
    if (inputSourceRef.current === "keyboard") {
      highlightElRef.current?.scrollIntoView({ block: "nearest" });
    }
  }, [highlight]);

  // Preview card driver: once the highlighted entry (keyboard and mouse
  // alike) has rested for previewDelay, push its content; the card decides
  // when to show by calling back into show_preview after measuring, so the
  // window height follows the content (see PreviewCard). Switching the
  // highlight or refreshing the list resets the timer, so rows swept through
  // are never pushed; if the panel is already closed, show_preview_impl on
  // the Rust side refuses to show.
  const highlightEntry = filtered.length > 0 ? filtered[highlight] : undefined;
  useEffect(() => {
    if (!hasTauri) return;
    // This round invalidates older sequences: once the highlight has moved
    // on, a late text response for the old row must not emit
    const seq = ++previewSeqRef.current;
    if (!highlightEntry || previewSuppressed) {
      // Nothing to show (no search results / empty history) or the pointer is
      // off the rows: hide the card at once
      api.hidePreview().catch(console.error);
      return;
    }
    const timer = window.setTimeout(() => {
      const anchorY = highlightElRef.current?.getBoundingClientRect().top ?? null;
      const send = (text: string | null, textTruncated: boolean) => {
        if (previewSeqRef.current !== seq) return;
        const payload: PreviewPayload = {
          entry: highlightEntry,
          anchorY,
          lang,
          text,
          textTruncated,
        };
        void emitTo("preview", EVENTS.PREVIEW_ENTRY, payload);
      };
      // Text entries: the list payload carries no full text, so fetch it once
      // the hover is confirmed (Rust already truncated it to the render
      // limit, and a 1-2ms IPC hidden behind the hover delay is
      // imperceptible); on failure fall back to null and the card shows the
      // preview instead
      if (highlightEntry.kind === "text") {
        api
          .historyEntryText(highlightEntry.id)
          .then((slice) => send(slice.text, slice.truncated))
          .catch(() => send(null, false));
      } else {
        send(null, false);
      }
    }, previewDelay);
    return () => clearTimeout(timer);
  }, [highlightEntry, lang, previewDelay, previewSuppressed]);

  /** Mouse movement inside the panel: hide the preview card whenever the
   *  pointer is off the **row band** — the search row, the footer menu and
   *  the blank tail of the list must not keep showing the first/last entry.
   *
   *  The decision is vertical only (probe at the list container's horizontal
   *  midline, ignoring the pointer's x): the container has horizontal
   *  padding, and deciding by the element under the pointer would read
   *  "moved sideways out of the row to go scroll the preview card" as leaving
   *  the row, so the card would vanish before the pointer got there */
  const onPanelMouseMove = useCallback((e: React.MouseEvent) => {
    const list = listRef.current;
    if (!list) return;
    const rect = list.getBoundingClientRect();
    const probe = document.elementFromPoint(rect.left + rect.width / 2, e.clientY);
    setPreviewSuppressed(!probe?.closest("[data-row]"));
  }, []);

  /** Hide the panel: clear the search state first so the next open's first
   *  frame is clean and never flashes the old filtered view, then go through
   *  the Rust side, which hides the preview card along with it and on macOS
   *  hands focus back so the paste lands */
  const dismiss = useCallback(() => {
    setQuery("");
    setSelected(0);
    api.hidePanel().catch(() => void getCurrentWindow().hide());
  }, []);

  /** Choose an entry: write it to the clipboard and hide the panel */
  const choose = useCallback(
    async (entry: HistoryEntryDto) => {
      try {
        await api.copyHistoryEntry(entry.id);
        setError("");
        dismiss();
      } catch (e) {
        setError(formatError(e));
      }
    },
    [dismiss],
  );

  /** Pointer moved onto a row (a stable callback for the memoized row
   *  component; React skips a same-value setState) */
  const hoverRow = useCallback((index: number) => {
    inputSourceRef.current = "mouse";
    setSelected((prev) => (prev === index ? prev : index));
  }, []);

  /** Pin / unpin (optimistic: flip locally, HISTORY_CHANGED corrects the
   *  ordering) */
  const togglePin = useCallback((entry: HistoryEntryDto) => {
    setEntries(
      (list) => list?.map((x) => (x.id === entry.id ? { ...x, pinned: !x.pinned } : x)) ?? null,
    );
    api.pinHistoryEntry(entry.id, !entry.pinned).catch((err) => setError(formatError(err)));
  }, []);

  /** Delete a single entry (optimistic: remove locally for instant
   *  feedback) */
  const remove = useCallback((entry: HistoryEntryDto) => {
    setEntries((list) => list?.filter((x) => x.id !== entry.id) ?? null);
    api.deleteHistoryEntry(entry.id).catch((err) => setError(formatError(err)));
  }, []);

  /** Clear the history once the dialog confirms (failures land on the error
   *  row at the bottom of the panel) */
  const confirmClear = useCallback(() => {
    setClearAsking(false);
    api.clearHistory().catch((err) => setError(formatError(err)));
  }, []);

  const onKeyDown = (e: React.KeyboardEvent) => {
    // While the confirmation is up the panel answers no keys: Esc cancels the
    // dialog rather than hiding the panel, everything else is swallowed —
    // the search field still holds focus, so without this you would type and
    // navigate the list underneath the dialog
    if (clearAsking) {
      e.preventDefault();
      if (e.key === "Escape") setClearAsking(false);
      return;
    }
    // Keys during IME composition belong to the input method (Enter commits a
    // candidate, ↑↓ pick one, Esc cancels the composition) and must not be
    // treated as list navigation — otherwise the panel disappears halfway
    // through typing a Chinese search
    if (e.nativeEvent.isComposing) return;
    // Shift (or Cmd, the macOS document convention) turns an arrow into a
    // jump to the far end of the list. It cannot be expressed as an arrow
    // with a large step: the step is relative to the current row, and jumping
    // has to land on an end regardless of where the highlight sits
    const jump = e.shiftKey || e.metaKey;
    if (e.key === "Escape") {
      dismiss();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      inputSourceRef.current = "keyboard";
      // Keyboard navigation is explicit intent: lift the suppression caused
      // by the pointer resting off the rows
      setPreviewSuppressed(false);
      setSelected((s) => (jump ? filtered.length - 1 : Math.min(s + 1, filtered.length - 1)));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      inputSourceRef.current = "keyboard";
      setPreviewSuppressed(false);
      setSelected((s) => (jump ? 0 : Math.max(s - 1, 0)));
    } else if (e.key === "Enter" && filtered[highlight]) {
      void choose(filtered[highlight]);
    }
  };

  return (
    <div
      className="relative flex h-screen flex-col overflow-hidden rounded-xl border border-line-2 bg-panel"
      onKeyDown={onKeyDown}
      onMouseMove={onPanelMouseMove}
    >
      {/* Search row (Maccy-style borderless input) */}
      <div className="flex shrink-0 items-center gap-2 border-b border-line px-3.5 py-2.5">
        <SearchIcon />
        <input
          ref={inputRef}
          autoFocus
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setSelected(0);
          }}
          placeholder={t.history.searchPlaceholder}
          className="min-w-0 flex-1 bg-transparent text-sm text-fog outline-none placeholder:text-faint"
        />
        {/* Entry count (its old footer spot went to the menu area) */}
        <span className="font-gauge shrink-0 text-[10px] text-faint">
          {entries === null ? "" : t.history.count(filtered.length)}
        </span>
      </div>

      {/* Entry list (entries === null means loading: render blank rather than
          the empty-state copy) */}
      <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1.5">
        {entries === null ? null : filtered.length === 0 ? (
          <div className="px-4 py-10 text-center text-xs text-mist">
            {loaded.length === 0 ? t.history.empty : t.history.noMatch}
          </div>
        ) : (
          <>
            {visibleRows.map((entry, index) => (
              <PanelRow
                key={entry.id}
                entry={entry}
                index={index}
                active={index === highlight}
                // Slot badges only appear without a search term — Alt+N takes
                // the Nth entry of the unfiltered ordering, so showing badges
                // while filtering would mislead
                slot={!query && index < 6 ? `${SLOT_MOD}${index + 1}` : ""}
                t={t}
                highlightElRef={highlightElRef}
                onHover={hoverRow}
                onChoose={choose}
                onPin={togglePin}
                onDelete={remove}
              />
            ))}
            {/* Scroll sentinel: mounted at the tail while the render window
                does not cover everything; entering view widens it */}
            {hasMoreRows && <div ref={sentinelRef} className="h-px" />}
          </>
        )}
      </div>

      {/* Footer menu (Maccy-style: the old tray context menu moved into the
          panel; Linux keeps a native tray menu as well). At the list's own
          rhythm it reads as one block, so it is separated three ways: a
          rule one shade darker + a footer tint (just a pale band under
          light, dark and vibrancy alike) + one extra step of top spacing */}
      <div className="shrink-0 border-t border-line-2 bg-abyss/50 px-1.5 pt-2.5 pb-2">
        {error && <div className="px-2.5 pb-1 text-[10px] text-alert">{error}</div>}
        <MenuRow label={t.history.clear} onClick={() => setClearAsking(true)} />
        {/* Menu items that open a window must **wait for the window to be
            shown before hiding the panel**: hiding the panel calls app.hide()
            on macOS to step aside entirely, and that decides based on whether
            any window is visible right now — two concurrent IPC calls have no
            ordering guarantee, so a hide that lands first takes the new
            window down with it (looks like "the window pops up and vanishes,
            then comes back when you click the tray again") */}
        <MenuRow
          label={t.history.settings}
          onClick={() => void api.showSettingsWindow().catch(console.error).finally(dismiss)}
        />
        <MenuRow
          label={t.history.about}
          onClick={() => void api.showAbout().catch(console.error).finally(dismiss)}
        />
        <MenuRow label={t.history.quit} onClick={() => api.quitApp().catch(console.error)} />
      </div>

      {clearAsking && (
        <ClearConfirm
          count={loaded.length}
          onCancel={() => setClearAsking(false)}
          onConfirm={confirmClear}
        />
      )}
    </div>
  );
}

/** In-panel clear-history confirmation (not a native dialog)
 *
 *  absolute rather than ModalShell's fixed: a fixed element is not clipped by
 *  ancestor overflow, so the square-cornered scrim would poke out past the
 *  panel's rounded corners. Clicking the scrim cancels — a destructive action
 *  gets an escape route by default */
function ClearConfirm({
  count,
  onCancel,
  onConfirm,
}: {
  count: number;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useI18n();
  return (
    <div
      onClick={onCancel}
      className="anim-fade-in absolute inset-0 z-50 flex items-center justify-center bg-abyss/70 p-5 backdrop-blur-[3px]"
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className="anim-fade-up w-full rounded-xl border border-line-2 bg-panel px-4 py-3.5 shadow-[0_18px_60px_rgba(0,0,0,0.45)]"
      >
        <div className="text-sm text-fog">{t.history.clearConfirm}</div>
        <div className="mt-1.5 text-[11px] leading-relaxed text-mist">
          {t.history.clearHint(count)}
        </div>
        <div className="mt-3.5 flex items-center justify-end gap-2">
          <Button onClick={onCancel}>{t.history.cancel}</Button>
          <Button variant="danger" onClick={onConfirm}>
            {t.history.clear}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** Footer menu row (Maccy-style): label + optional trailing element such as a
 *  toggle. The text sits one level below the list entries — the menu is
 *  chrome, not content — and only rises to the primary colour on hover */
function MenuRow({
  label,
  right,
  onClick,
}: {
  label: string;
  right?: React.ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-1 text-left text-[12px] text-mist transition-colors hover:bg-sonar/10 hover:text-fog"
    >
      <span className="min-w-0 truncate">{label}</span>
      {right}
    </button>
  );
}


/** One list row (memoized: moving the highlight re-renders only the two rows
 *  involved — re-rendering all 200 rows is the direct cause of stutter while
 *  sweeping the mouse; every callback must be a stable reference or the memo
 *  is useless) */
const PanelRow = memo(function PanelRow({
  entry,
  index,
  active,
  slot,
  t,
  highlightElRef,
  onHover,
  onChoose,
  onPin,
  onDelete,
}: {
  entry: HistoryEntryDto;
  index: number;
  active: boolean;
  slot: string;
  t: Locale;
  highlightElRef: React.RefObject<HTMLDivElement | null>;
  onHover: (index: number) => void;
  onChoose: (entry: HistoryEntryDto) => void;
  onPin: (entry: HistoryEntryDto) => void;
  onDelete: (entry: HistoryEntryDto) => void;
}) {
  return (
    <div
      // Row-band marker: the panel-level mousemove uses this to decide
      // whether the pointer is on an entry (onPanelMouseMove)
      data-row=""
      onClick={() => onChoose(entry)}
      // mousemove rather than mouseenter: keyboard scrolling slides a new row
      // under a stationary pointer and fires mouseenter, stealing the
      // highlight; only real movement produces mousemove
      onMouseMove={() => onHover(index)}
      ref={
        active
          ? (el) => {
              highlightElRef.current = el;
            }
          : undefined
      }
      className={`group flex cursor-pointer items-center gap-2 rounded-lg px-2.5 py-[5px] ${
        active ? "bg-sonar text-white" : "text-fog"
      }`}
    >
      {entry.kind !== "text" && <KindIcon kind={entry.kind} active={active} />}
      <span className="min-w-0 flex-1 truncate text-[13px]">
        {entry.kind === "image" ? t.history.imageLabel(entry.preview) : entry.preview || "␣"}
      </span>
      {entry.pinned && (
        <span className={`shrink-0 text-[10px] ${active ? "text-white" : "text-sonar"}`}>●</span>
      )}
      {slot && (
        <span
          className={`font-gauge shrink-0 text-[10px] group-hover:hidden ${
            active ? "text-white/70" : "text-faint"
          }`}
        >
          {slot}
        </span>
      )}
      {/* Hover actions: pin / delete (they swap places with the slot badge,
          so the row width never jumps) */}
      <div className="hidden shrink-0 items-center gap-0.5 group-hover:flex">
        <button
          title={entry.pinned ? t.history.unpin : t.history.pin}
          onClick={(e) => {
            e.stopPropagation();
            onPin(entry);
          }}
          className={`cursor-pointer rounded p-0.5 ${
            active ? "text-white/80 hover:text-white" : "text-mist hover:text-sonar"
          }`}
        >
          <PinIcon />
        </button>
        <button
          title={t.history.delete}
          onClick={(e) => {
            e.stopPropagation();
            onDelete(entry);
          }}
          className={`cursor-pointer rounded p-0.5 text-xs leading-none ${
            active ? "text-white/80 hover:text-white" : "text-mist hover:text-alert"
          }`}
        >
          ✕
        </button>
      </div>
    </div>
  );
});

/** Kind icon (image and file rows only; text rows carry none, Maccy-style
 *  whitespace) */
function KindIcon({ kind, active }: { kind: string; active: boolean }) {
  const cls = `size-3.5 shrink-0 ${active ? "text-white/80" : "text-mist"}`;
  if (kind === "image") {
    return (
      <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <circle cx="8.5" cy="8.5" r="1.5" fill="currentColor" stroke="none" />
        <path d="m21 15-5-5L5 21" />
      </svg>
    );
  }
  return (
    <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />
      <path d="M13 2v7h7" />
    </svg>
  );
}

/** Pin action icon */
function PinIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M12 17v5M9 3h6l1 7 2 2H6l2-2z" />
    </svg>
  );
}

/** Search magnifier icon */
function SearchIcon() {
  return (
    <svg
      className="size-3.5 shrink-0 text-faint"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
    >
      <circle cx="11" cy="11" r="7" />
      <path d="m21 21-4.3-4.3" />
    </svg>
  );
}
