// About window (a small standalone window, the same shape Maccy uses): app
// identity + version + source link + copyright
//
// Deliberately no "check for updates" — the app has no update mechanism, so
// the button would be dead. No local fingerprint either: it sits in the
// settings window's pinned footer and is visible whenever you pair, so
// repeating it here buys nothing.

import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../api";
import { useI18n } from "../i18n";
import { useTheme } from "../theme";

/** Whether we are running inside the Tauri runtime */
const hasTauri = "__TAURI_INTERNALS__" in window;

/** Project home page (capabilities allowlists opener for exactly this URL, so
 *  changing it means updating default.json too) */
const REPO_URL = "https://github.com/zlx2019/lanecho";

/** About window */
export function AboutWindow() {
  const { t } = useI18n();
  // A separate document applies the theme itself (the localStorage key is
  // shared, and the storage event follows the main window)
  useTheme();
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    if (!hasTauri) return;
    api.appVersion().then(setVersion).catch(console.error);
  }, []);

  return (
    <div className="flex h-screen flex-col items-center justify-center gap-1 px-6 text-center">
      <img src="/logo.svg" alt="" className="h-16 w-16" />
      <div className="mt-2 text-lg text-fog">lanecho</div>
      <div className="text-[11px] text-mist">{t.about.tagline}</div>

      <div className="font-gauge mt-3 text-xs text-mist">
        {t.about.version} {version ?? "—"}
      </div>

      <button
        onClick={() => void openUrl(REPO_URL).catch(console.error)}
        className="mt-3 cursor-pointer text-xs text-sonar underline-offset-2 transition-colors hover:underline"
      >
        {t.about.repo}
      </button>

      <div className="mt-4 text-[11px] text-faint">{t.about.license}</div>
      <div className="text-[11px] text-faint">{t.about.copyright}</div>
    </div>
  );
}
