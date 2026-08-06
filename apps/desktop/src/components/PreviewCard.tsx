// Preview card (Maccy-style): the full content of the highlighted history
// entry plus its metadata
//
// Its own WebView window (label = "preview"): a pure presentation layer with
// focusable=false, positioned and shown/hidden on the Rust side (lib.rs
// show_preview_impl). The panel pushes the entry plus the row anchor over the
// PREVIEW_ENTRY event; after rendering, this component **measures the actual
// content height** and calls show_preview back — the window height follows
// the content, so short text is a small card rather than an empty slab.
// When the content exceeds the height cap the Rust side takes back mouse
// pass-through, so the content area here has to scroll.
//
// Performance rules (the dual of HistoryPanel's):
// - Image Blob URLs are LRU-cached by entry id: sweeping the highlight back
//   onto the same image re-runs neither the IPC nor the decode; the backend
//   already downsamples to display size, so the cache holds only small images
// - Language comes from the payload (the panel has already aligned it), so
//   there is no re-reading getSettings on every push
// - Text arrives already truncated to the render limit on the Rust side; the
//   truncation notice keys off textTruncated

import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import { useI18n } from "../i18n";
import { useTheme } from "../theme";
import type { HistoryEntryDto, PreviewPayload } from "../types";

/** Whether we are running inside the Tauri runtime */
const hasTauri = "__TAURI_INTERNALS__" in window;

/** Text render limit (characters): the truncation itself happens Rust-side
 *  (commands' PREVIEW_TEXT_MAX_CHARS, the two must stay equal); this constant
 *  only feeds the truncation notice */
const PREVIEW_TEXT_LIMIT = 20000;

/** Image Blob URL cache cap (entries): anything past it is evicted LRU and
 *  its URL revoked — recently swept images stay cached so moving the
 *  highlight back and forth does not refetch, and the cap keeps a long
 *  session's memory from growing without bound */
const IMG_CACHE_CAP = 12;

/** Image display height cap (logical pixels): together with the metadata area
 *  it stays under the window height cap of 480, and taller images scale down
 *  proportionally — seeing an image whole beats scrolling through it, so none
 *  of it is left to the scroller */
const MAX_IMAGE_HEIGHT = 330;

/** Preview card */
export function PreviewCard() {
  const { t, lang, setLang } = useI18n();
  // A separate document applies the theme itself (the localStorage key is
  // shared, and the storage event follows the main window)
  useTheme();
  const [entry, setEntry] = useState<HistoryEntryDto | null>(null);
  const [anchorY, setAnchorY] = useState<number | null>(null);
  const [text, setText] = useState<string | null>(null);
  const [textTruncated, setTextTruncated] = useState(false);
  const [imgUrl, setImgUrl] = useState<string | null>(null);
  const [imgReady, setImgReady] = useState(false);
  const [appIconUrl, setAppIconUrl] = useState<string | null>(null);
  // Current entry ref (async callbacks use it to tell whether the highlight
  // has already moved on)
  const entryRef = useRef<HistoryEntryDto | null>(null);
  entryRef.current = entry;
  // Image Blob URL cache (entry id → URL; Map iteration order is the LRU
  // order) and the set of in-flight requests
  const imgCacheRef = useRef<Map<string, string>>(new Map());
  const imgPendingRef = useRef<Set<string>>(new Set());
  // App icon cache (app name → Blob URL; the "" sentinel means there is no
  // cached icon, so stop asking)
  const iconCacheRef = useRef<Map<string, string>>(new Map());
  // Measurement targets: the inner content div (natural height, not stretched
  // by flex) and the metadata area
  const contentRef = useRef<HTMLDivElement>(null);
  const metaRef = useRef<HTMLDivElement>(null);
  // Content scroller (used when the window height cap clips long content)
  const scrollRef = useRef<HTMLDivElement>(null);

  // Switch to the translucent variables when vibrancy is active (same rule as
  // HistoryPanel)
  useEffect(() => {
    if (!hasTauri) return;
    api
      .windowEffectsActive()
      .then((active) => {
        if (active) document.documentElement.dataset.vibrancy = "1";
      })
      .catch(console.error);
  }, []);

  // Receive the highlighted entry pushed by the panel (language aligns from
  // the payload; this long-lived hidden document is not rebuilt when the main
  // window is)
  useEffect(() => {
    if (!hasTauri) return;
    let alive = true;
    let unsub: UnlistenFn | null = null;
    listen<PreviewPayload>(EVENTS.PREVIEW_ENTRY, (e) => {
      if (!alive) return;
      const { entry, anchorY, lang, text, textTruncated } = e.payload;
      setEntry(entry);
      setAnchorY(anchorY);
      setText(text);
      setTextTruncated(textTruncated);
      if (lang === "zh" || lang === "en") setLang(lang);
    })
      .then((fn) => {
        if (alive) {
          unsub = fn;
        } else {
          fn();
        }
      })
      .catch(console.error);
    return () => {
      alive = false;
      unsub?.();
    };
  }, [setLang]);

  // Image entries: try the cache first; only a miss pulls the PNG over the
  // binary IPC into a Blob URL
  useEffect(() => {
    if (!entry || entry.kind !== "image") {
      setImgUrl(null);
      setImgReady(false);
      return;
    }
    const cache = imgCacheRef.current;
    const cached = cache.get(entry.id);
    if (cached) {
      // LRU touch (Map iteration order is insertion order, so delete then
      // re-insert moves it to the tail)
      cache.delete(entry.id);
      cache.set(entry.id, cached);
      if (imgUrl !== cached) {
        setImgReady(false);
        setImgUrl(cached);
      }
      return;
    }
    // A repeat push for the same entry (a copy-count refresh) may find the
    // request still in flight; do not issue it twice
    if (imgPendingRef.current.has(entry.id)) return;
    imgPendingRef.current.add(entry.id);
    setImgUrl(null);
    setImgReady(false);
    const id = entry.id;
    api
      .historyImagePng(id)
      .then((buf) => {
        imgPendingRef.current.delete(id);
        const url = URL.createObjectURL(new Blob([buf], { type: "image/png" }));
        cache.set(id, url);
        if (cache.size > IMG_CACHE_CAP) {
          const oldest = cache.keys().next().value;
          if (oldest !== undefined && oldest !== id) {
            const evicted = cache.get(oldest);
            cache.delete(oldest);
            if (evicted) URL.revokeObjectURL(evicted);
          }
        }
        // The highlight moved on while we waited: keep the cache entry but do
        // not update what is displayed
        if (entryRef.current?.id === id) setImgUrl(url);
      })
      .catch(() => {
        imgPendingRef.current.delete(id);
        // Fetch failed (a corrupted blob, say): show metadata only, and set
        // imgReady so the card still pops as usual
        if (entryRef.current?.id === id) {
          setImgUrl(null);
          setImgReady(true);
        }
      });
  }, [entry, imgUrl]);

  // Source application icon: fetched by app name from the backend appicons
  // directory, with the Blob URL reused for the rest of the session
  useEffect(() => {
    const name = entry?.sourceApp;
    if (!hasTauri || !name) {
      setAppIconUrl(null);
      return;
    }
    const cached = iconCacheRef.current.get(name);
    if (cached !== undefined) {
      setAppIconUrl(cached || null);
      return;
    }
    api
      .appIconPng(name)
      .then((buf) => {
        const url = URL.createObjectURL(new Blob([buf], { type: "image/png" }));
        iconCacheRef.current.set(name, url);
        // The entry changed while we waited: keep the cache entry but do not
        // update what is displayed
        if (entryRef.current?.sourceApp === name) setAppIconUrl(url);
      })
      .catch(() => {
        // No cached icon for this app (an old entry, or capture failed):
        // record the sentinel so we stop asking
        iconCacheRef.current.set(name, "");
        if (entryRef.current?.sourceApp === name) setAppIconUrl(null);
      });
  }, [entry]);

  // Revoke every Blob URL on unmount (this document is long-lived, so in
  // practice that only happens at process exit)
  useEffect(() => {
    const images = imgCacheRef.current;
    const icons = iconCacheRef.current;
    return () => {
      images.forEach((url) => URL.revokeObjectURL(url));
      icons.forEach((url) => url && URL.revokeObjectURL(url));
    };
  }, []);

  // Reset the scroll position when the entry changes: the previous one may
  // have been scrolled to the bottom, and a new entry should not open at that
  // old offset (a repeat push for the same entry — a copy-count refresh —
  // does not reset, so the spot the user is reading never jumps)
  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
  }, [entry?.id]);

  // Measure once the content is ready, then call back to show it: height =
  // natural content height + content padding (24) + metadata height + top and
  // bottom borders (2); the Rust side then clamps to [100, 480].
  // Images must wait for onLoad (imgReady) before measuring — offsetHeight is
  // 0 until the image has loaded
  useEffect(() => {
    if (!hasTauri || !entry) return;
    if (entry.kind === "image" && !imgReady) return;
    const contentH = contentRef.current?.offsetHeight ?? 0;
    const metaH = metaRef.current?.offsetHeight ?? 0;
    api.showPreview(anchorY, contentH + 24 + metaH + 2).catch(console.error);
  }, [entry, anchorY, imgReady]);

  // Nothing yet (no first push since startup): paint an empty backdrop, the
  // window is hidden at this point anyway
  if (!entry) {
    return <div className="h-screen rounded-xl border border-line-2 bg-panel" />;
  }

  // Fall back to preview when the text fetch failed (the entry was just
  // deleted, say) so the card is never blank
  const bodyText = text ?? entry.preview;
  const truncated = entry.kind === "text" && textTruncated;

  return (
    <div className="flex h-screen flex-col overflow-hidden rounded-xl border border-line-2 bg-panel">
      {/* Content area: full text / the image / the list of file paths (the
          inner div is there so the natural height can be measured). When the
          content exceeds the cap the window takes back mouse pass-through and
          the wheel scrolls here; overscroll-contain kills the rubber-band
          bounce at the end so the whole card does not wobble */}
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto overscroll-contain p-3">
        <div ref={contentRef}>
          {entry.kind === "image" ? (
            imgUrl && (
              <img
                src={imgUrl}
                alt=""
                // Decode off the main thread: decoding a large image
                // synchronously stalls the measurement and the show callback
                decoding="async"
                onLoad={() => setImgReady(true)}
                style={{ maxHeight: MAX_IMAGE_HEIGHT }}
                className="mx-auto max-w-full rounded-md object-contain"
              />
            )
          ) : entry.kind === "files" ? (
            <div className="space-y-1">
              {(entry.files ?? []).map((path) => (
                <div
                  key={path}
                  className="font-gauge text-[11px] leading-relaxed break-all text-fog"
                >
                  {path}
                </div>
              ))}
            </div>
          ) : (
            <>
              <pre className="font-gauge m-0 text-[11px] leading-relaxed break-words whitespace-pre-wrap text-fog">
                {bodyText}
              </pre>
              {truncated && (
                <div className="mt-2 text-[10px] text-faint">
                  {t.preview.truncated(PREVIEW_TEXT_LIMIT)}
                </div>
              )}
            </>
          )}
        </div>
      </div>

      {/* Metadata area (Maccy-style): source application / origin device /
          first and last copied / copy count */}
      <div
        ref={metaRef}
        className="shrink-0 space-y-1 border-t border-line px-3.5 py-2.5 text-[11px]"
      >
        {entry.sourceApp && (
          <div className="flex gap-2">
            <span className="w-20 shrink-0 text-mist">{t.preview.app}</span>
            <span className="flex min-w-0 flex-1 items-center gap-1.5 text-fog">
              {appIconUrl && (
                <img src={appIconUrl} alt="" className="size-3.5 shrink-0 rounded-[3px]" />
              )}
              <span className="truncate">{entry.sourceApp}</span>
            </span>
          </div>
        )}
        {entry.origin && <MetaRow label={t.preview.fromDevice} value={entry.origin} />}
        <MetaRow label={t.preview.firstCopied} value={fmtTime(entry.firstCopiedAt, lang)} />
        <MetaRow label={t.preview.lastCopied} value={fmtTime(entry.lastCopiedAt, lang)} />
        <MetaRow label={t.preview.copyCount} value={String(entry.copyCount)} />
      </div>
    </div>
  );
}

/** Metadata row: label + value */
function MetaRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <span className="w-20 shrink-0 text-mist">{label}</span>
      <span className="min-w-0 flex-1 truncate text-fog">{value}</span>
    </div>
  );
}

/** Time formatters cached per language (constructing Intl.DateTimeFormat has
 *  a noticeable cost, and building a new one for each of the two time rows on
 *  every render is pure waste; with only two languages the Map holds two
 *  instances) */
const timeFormatters = new Map<string, Intl.DateTimeFormat>();

/** Format a timestamp in the current language
 *  (zh "2026/8/3 16:26" / en "Aug 3, 2026, 16:26") */
function fmtTime(ms: number, lang: string): string {
  let formatter = timeFormatters.get(lang);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(lang === "zh" ? "zh-CN" : "en-US", {
      year: "numeric",
      month: lang === "zh" ? "long" : "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
    timeFormatters.set(lang, formatter);
  }
  return formatter.format(new Date(ms));
}
