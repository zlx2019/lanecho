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
