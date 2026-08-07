// Three-state theme hook (system / light / dark)
//
// - The preference lives in localStorage("lanecho-theme"); only light and
//   dark count as explicit values, anything else reads as system
// - "Follow system" keeps a matchMedia listener alive, so a system switch
//   takes effect immediately
// - The native window chrome is kept in step through setTheme; system passes
//   null to hand the decision back (Tauri's setTheme only accepts
//   "light" | "dark" | null)

import { useCallback, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** The three theme preferences */
export type ThemePref = "system" | "light" | "dark";

const THEME_KEY = "lanecho-theme";

/** Document backdrop per theme, painted as an inline style
 *
 *  **Same two values as the inline script in index.html** — that script runs
 *  before the stylesheet has loaded, which is the whole reason the backdrop is
 *  an inline style rather than CSS, so the pair has to be kept in step by
 *  hand. */
const BACKDROP = { dark: "#101425", light: "#eef1fa" } as const;

/** The system's current light/dark state (tests for light; environments
 *  without matchMedia fall back to dark) */
function systemTheme(): "light" | "dark" {
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** Read the persisted preference */
function storedPref(): ThemePref {
  const saved = localStorage.getItem(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : "system";
}

/** Theme state plus cycling through the three (system → light → dark →
 *  system) */
export function useTheme() {
  const [pref, setPref] = useState<ThemePref>(storedPref);
  const [sysTheme, setSysTheme] = useState<"light" | "dark">(systemTheme);

  // Keep listening for the system's light/dark state: under "follow system" a
  // switch takes effect immediately
  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-color-scheme: light)");
    if (!mq) return;
    const onChange = () => setSysTheme(systemTheme());
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  // Cross-window sync: the floating history panel is a long-lived hidden
  // document, so when the main window switches theme and writes localStorage
  // it follows over the storage event (same-origin windows share
  // localStorage)
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === THEME_KEY) setPref(storedPref());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  const theme = pref === "system" ? sysTheme : pref;

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    // Repaint the backdrop index.html put on the document inline. An inline
    // style outranks every stylesheet rule, so left alone it keeps the theme
    // the window was *loaded* with: switch to light, reopen a window that was
    // created while dark, and the stale backdrop paints one dark frame before
    // the light content composites over it. Every window is long-lived and
    // only hidden, so this is on every reopen, not just the first.
    // Floating windows are exempt — their documents stay transparent so the
    // rounded root container can paint the background (see main.tsx)
    if (!document.documentElement.dataset.floating) {
      document.documentElement.style.background = BACKDROP[theme];
    }
    localStorage.setItem(THEME_KEY, pref);
    // Keep the native window chrome in step; system passes null to hand the
    // decision back, so the title bar never keeps the old theme
    if ("__TAURI_INTERNALS__" in window) {
      getCurrentWindow()
        .setTheme(pref === "system" ? null : pref)
        .catch(console.error);
    }
  }, [theme, pref]);

  const cycle = useCallback(() => {
    setPref((p) => (p === "system" ? "light" : p === "light" ? "dark" : "system"));
  }, []);

  return { pref, theme, cycle, setPref };
}
