// i18n foundation: LocaleProvider + useI18n (for components) + getLocale (for
// non-component modules)
//
// Copy files: zh.ts is the authoritative key source, en.ts uses
// `satisfies Locale` to guarantee the keys are complete.
// The language preference lives in settings (settings.language); it is empty
// on a first launch, so the system language is detected and written back, and
// from then on settings decide — the settings area can switch it at any time
// and a save takes effect immediately, tray and notifications on the Rust
// side included.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import { api } from "../api";
import { zh } from "./zh";
import { en } from "./en";

/** Type of the copy table: the Chinese file's shape defines it */
export type Locale = typeof zh;
/** Supported languages */
export type Lang = "zh" | "en";

const LOCALES: Record<Lang, Locale> = { zh, en };

// Module-level mirror so non-component contexts can read the current copy
// (kept in step whenever the provider switches)
let current: { lang: Lang; t: Locale } = { lang: "zh", t: zh };

/** The current copy table (for non-component modules; inside a component use
 *  useI18n so a switch re-renders it) */
export function getLocale(): Locale {
  return current.t;
}

/** Pick the default language from the system: Chinese systems get zh,
 * everything else en (the ?lang=en query parameter overrides it for visual
 * debugging in a browser; inside Tauri there is no query string) */
export function detectSystemLang(): Lang {
  const forced = new URLSearchParams(window.location.search).get("lang");
  if (forced === "zh" || forced === "en") return forced;
  return navigator.language?.toLowerCase().startsWith("zh") ? "zh" : "en";
}

interface I18nValue {
  lang: Lang;
  t: Locale;
  setLang: (lang: Lang) => void;
}

const I18nContext = createContext<I18nValue>({
  lang: current.lang,
  t: current.t,
  setLang: () => {},
});

/** Language context: read from settings at startup (when empty, detect from
 *  the system language and persist that back) */
export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [lang, setLangState] = useState<Lang>(current.lang);

  const setLang = useCallback((next: Lang) => {
    current = { lang: next, t: LOCALES[next] };
    setLangState(next);
  }, []);

  useEffect(() => {
    // A plain browser preview (no Tauri) cannot read settings, so it just
    // shows the system language
    if (!("__TAURI_INTERNALS__" in window)) {
      setLang(detectSystemLang());
      return;
    }
    api
      .getSettings()
      .then((s) => {
        const saved = s.language === "zh" || s.language === "en" ? s.language : null;
        const lang = saved ?? detectSystemLang();
        setLang(lang);
        // Write the detected value back on first launch: the tray and
        // notifications (Rust side) and every later launch read from settings
        if (!saved) {
          api.saveSettings({ ...s, language: lang }).catch(console.error);
        }
      })
      .catch(console.error);
  }, [setLang]);

  const value = useMemo(() => ({ lang, t: LOCALES[lang], setLang }), [lang, setLang]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

/** Get the current language and copy table */
export function useI18n(): I18nValue {
  return useContext(I18nContext);
}

/** Look up an error code's copy in the current language; an unknown code
 *  returns null and the caller falls back to the raw string */
function errorText(code: string, detail?: string | null): string | null {
  const table = getLocale().errors as Record<string, string | undefined>;
  const msg = table[code];
  if (!msg) return null;
  return detail ? `${msg} (${detail})` : msg;
}

/** A structured backend error (ErrDto), or any exception, rendered as display
 *  copy in the current language */
export function formatError(e: unknown): string {
  if (e && typeof e === "object" && "code" in e) {
    const { code, detail } = e as { code: string; detail?: string };
    const msg = errorText(code, detail);
    if (msg) return msg;
    // A code i18n does not carry (a new one on the Rust side whose copy was
    // missed): show the raw code rather than landing on String(e)'s
    // "[object Object]"
    return detail ? `${code}: ${detail}` : code;
  }
  return String(e);
}
