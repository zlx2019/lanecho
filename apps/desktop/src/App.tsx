// lanecho main window: tab navigation across the top (general / sync /
// storage / hotkeys) + a pinned footer carrying the local fingerprint.
// The control panel of a tray-resident app: no radar, no drag and drop —
// deliberately minimal.
//
// The window height follows the current tab's content (ResizeObserver →
// resize_settings_window): there are not many settings, and a fixed large
// window would be mostly empty. The width is left alone, so a width the user
// dragged to is preserved.
//
// Every setting applies the moment it changes, as in the native client:
// choices on click, typed fields on Enter / blur. There is no save button.

import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { EVENTS } from "./events";
import { DeviceList } from "./components/DeviceList";
import { HotkeyRecorder } from "./components/HotkeyRecorder";
import { IgnorePane } from "./components/IgnorePane";
import { PairRequestModal } from "./components/PairRequestModal";
import { ToggleRow } from "./components/ModalShell";
import { useLanecho } from "./hooks/useLanecho";
import { formatError, useI18n, type Lang } from "./i18n";
import { useTheme, type ThemePref } from "./theme";
import { SLOT_MODIFIER_OPTIONS, slotModLabel } from "./hotkeys";
import type { AutoPasteStatus, Settings } from "./types";

/** Whether we are running inside the Tauri runtime */
const hasTauri = "__TAURI_INTERNALS__" in window;

/** Language options */
const LANGS: [Lang, string][] = [
  ["zh", "中文"],
  ["en", "English"],
];

/** Settings tabs (general is the default; the order follows the flow of use:
 *  identity → find and pair devices → sync behaviour → storage → ignore
 *  rules → hotkeys) */
type Tab = "general" | "devices" | "sync" | "storage" | "ignore" | "hotkeys";
const TABS: Tab[] = ["general", "devices", "sync", "storage", "ignore", "hotkeys"];

/** Total vertical padding of the main area (kept in sync with main's py-4):
 *  used when computing the adaptive height */
const MAIN_PADDING_Y = 32;

export default function App() {
  const { lang, t, setLang } = useI18n();
  const { pref, cycle, setPref } = useTheme();

  const [tab, setTab] = useState<Tab>("general");
  const [settings, setSettings] = useState<Settings | null>(null);
  const lanecho = useLanecho({
    // Sync policy echo (from a Linux tray toggle or from saving settings):
    // the payload is the syncMode string and lands back verbatim — a bool
    // payload would collapse to both/off, so switching from off to send-only
    // or receive-only would rewrite the radio button to two-way
    onSyncState: (mode) =>
      setSettings((s) =>
        s ? { ...s, syncMode: mode, syncEnabled: mode !== "off" } : s,
      ),
  });

  // Latest settings for building patches: two commits inside one tick must
  // stack, not both start from the same render's snapshot
  const settingsRef = useRef<Settings | null>(null);
  settingsRef.current = settings;
  // Saves run one after another: each carries the whole object, so an older
  // snapshot landing last would undo a newer change
  const saveQueue = useRef<Promise<unknown>>(Promise.resolve());

  // Device name draft (committed on Enter / blur through its own command; Esc
  // sets the flag so the blur it causes reverts instead of committing)
  const [nickname, setNickname] = useState("");
  const nicknameCancelled = useRef(false);
  // Error from the last failed change, shown under the current tab (cleared
  // by the next successful change or a tab switch)
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const [incognito, setIncognito] = useState(false);
  const [usage, setUsage] = useState(0);
  const [slotFailures, setSlotFailures] = useState<number[]>([]);
  // Auto-paste availability: unsupported hides the whole row, unpermitted
  // keeps it but adds the permission hint (null until the first read lands)
  const [autoPaste, setAutoPaste] = useState<AutoPasteStatus | null>(null);
  // Measurement targets for the adaptive height: the shell (whole window) /
  // the main area (flex-1) / the inner content div (natural height)
  const shellRef = useRef<HTMLDivElement>(null);
  const mainRef = useRef<HTMLElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  // Timer that auto-hides the "copied" hint (unmount clears it)
  const copiedTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => clearTimeout(copiedTimer.current), []);

  // Initial load: settings + history usage + incognito state (a tray toggle
  // echoes back over an event)
  useEffect(() => {
    if (!hasTauri) return;
    let alive = true;
    api
      .getSettings()
      .then((s) => alive && setSettings(s))
      .catch(console.error);
    api
      .getIncognito()
      .then((v) => alive && setIncognito(v))
      .catch(console.error);
    api
      .historyUsage()
      .then((v) => alive && setUsage(v))
      .catch(console.error);
    api
      .getSlotHotkeyFailures()
      .then((v) => alive && setSlotFailures(v))
      .catch(console.error);
    api
      .autoPasteStatus()
      .then((v) => alive && setAutoPaste(v))
      .catch(console.error);
    const unsubs: (() => void)[] = [];
    const add = (subscription: Promise<() => void>) => {
      subscription
        .then((unsub) => (alive ? unsubs.push(unsub) : unsub()))
        .catch(console.error);
    };
    add(
      listen<boolean>(EVENTS.INCOGNITO_STATE, (e) => {
        if (alive) setIncognito(e.payload);
      }),
    );
    // Usage refresh: the settings window spends most of its time hidden
    // behind the tray, so skip the disk walk while hidden and catch up once
    // it is visible again (throttles background work on the copy path)
    const refreshUsage = () => {
      api
        .historyUsage()
        .then((v) => alive && setUsage(v))
        .catch(console.error);
    };
    add(
      listen(EVENTS.HISTORY_CHANGED, () => {
        if (document.visibilityState === "visible") refreshUsage();
      }),
    );
    const onVisible = () => {
      if (document.visibilityState === "visible") refreshUsage();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      alive = false;
      document.removeEventListener("visibilitychange", onVisible);
      unsubs.forEach((u) => u());
    };
  }, []);
  // The device name is filled in only on first load (or on an explicit
  // request after a save); it never overwrites what is being typed
  const nicknameSynced = useRef(false);
  useEffect(() => {
    if (lanecho.self && !nicknameSynced.current) {
      nicknameSynced.current = true;
      setNickname(lanecho.self.name);
    }
  }, [lanecho.self]);

  // Adaptive window height: whenever the content height changes (a tab
  // switch, devices coming and going, a conflict hint appearing), shrink the
  // window to exactly wrap the content. Fixed chrome height = shell height -
  // main height — main is flex-1, so that difference holds at any window
  // size and the header, nav and footer need no measuring of their own
  useEffect(() => {
    if (!hasTauri) return;
    const content = contentRef.current;
    if (!content) return;
    const apply = () => {
      const shellH = shellRef.current?.offsetHeight ?? 0;
      const mainH = mainRef.current?.offsetHeight ?? 0;
      if (!shellH || !mainH) return;
      const chrome = shellH - mainH;
      api
        .resizeSettingsWindow(chrome + content.offsetHeight + MAIN_PADDING_Y)
        .catch(console.error);
    };
    const observer = new ResizeObserver(apply);
    observer.observe(content);
    return () => observer.disconnect();
  }, []);

  // Tab switches replay a light fade on the content area, so the swap does
  // not read as a hard cut (restart by resetting the animation and forcing
  // one reflow; a keyed remount would detach the ResizeObserver above)
  useEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    content.style.animation = "none";
    void content.offsetHeight;
    content.style.animation = "";
    // An error belongs to the tab it happened on
    setError("");
  }, [tab]);

  /** Apply a change at once: update locally, persist in order. A failure
   *  (an unusable hotkey, a failed write) is reported and the page re-reads
   *  the settings, since the backend rolls the whole call back. Resolves
   *  after the backend has applied the change */
  const patchSettings = (patch: Partial<Settings>): Promise<void> => {
    const current = settingsRef.current;
    if (!current) return Promise.resolve();
    const next = { ...current, ...patch };
    settingsRef.current = next;
    setSettings(next);
    const run = saveQueue.current.then(() => api.saveSettings(next));
    saveQueue.current = run.catch(() => {});
    return run
      .then(() => setError(""))
      .catch((e) => {
        setError(formatError(e));
        api
          .getSettings()
          .then((s) => {
            settingsRef.current = s;
            setSettings(s);
          })
          .catch(console.error);
      });
  };

  /** Re-read which slot hotkeys failed to register (after any change that
   *  re-registers them) */
  const refreshSlotFailures = () => {
    api.getSlotHotkeyFailures().then(setSlotFailures).catch(console.error);
  };

  /** Commit the device name (Enter / blur): it goes through its own command,
   *  identity.json being its only source of truth */
  const commitNickname = () => {
    const self = lanecho.self;
    if (!self) return;
    const trimmed = nickname.trim();
    if (trimmed === self.name) {
      setNickname(self.name);
      return;
    }
    api
      .setDisplayName(trimmed || null)
      .then(() => {
        setError("");
        // Let the next self refresh fill the field in: clearing it falls back
        // to the hostname, and the actual new name has to show
        nicknameSynced.current = false;
        lanecho.refreshSelf();
      })
      .catch((e) => setError(formatError(e)));
  };

  /** Copy the local fingerprint */
  const copyFingerprint = () => {
    if (!lanecho.self) return;
    navigator.clipboard
      .writeText(lanecho.self.fingerprint)
      .then(() => {
        setCopied(true);
        clearTimeout(copiedTimer.current);
        copiedTimer.current = window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(console.error);
  };

  const themeTitle =
    pref === "system" ? t.header.toLight : pref === "light" ? t.header.toDark : t.header.toSystem;

  return (
    <div ref={shellRef} className="flex h-full flex-col">
      {/* Header */}
      <header className="flex shrink-0 items-center justify-between border-b border-line px-6 py-3">
        <div className="flex items-center gap-3">
          <img src="/logo.svg" className="size-8 rounded-lg" alt="" />
          <div>
            <div className="text-sm font-medium text-fog">Lanecho</div>
            <div className="text-[11px] text-mist">{t.header.tagline}</div>
          </div>
        </div>
        <button
          onClick={cycle}
          title={themeTitle}
          className="cursor-pointer rounded-md border border-line-2 p-1.5 text-mist transition-colors hover:border-mist hover:text-fog"
        >
          <ThemeIcon pref={pref} />
        </button>
      </header>

      {/* Tab navigation */}
      {/* gap-5: six English labels have to fit the 420px minimum width */}
      <nav className="flex shrink-0 gap-5 border-b border-line px-6">
        {TABS.map((item) => (
          <button
            key={item}
            onClick={() => setTab(item)}
            className={`-mb-px cursor-pointer border-b-2 pt-2.5 pb-2 text-sm transition-colors ${
              tab === item
                ? "border-sonar text-sonar"
                : "border-transparent text-mist hover:text-fog"
            }`}
          >
            {t.nav[item]}
          </button>
        ))}
      </nav>

      <main ref={mainRef} className="min-h-0 flex-1 overflow-y-auto px-6 py-4">
        <div ref={contentRef} className="anim-fade-in">
          {/* General tab: name / language / theme / launch at login */}
          {tab === "general" && (
            <div className="rounded-xl border border-line bg-panel px-4 py-3">
              <div className="gauge-label mb-1">{t.settings.nickname}</div>
              <input
                value={nickname}
                onChange={(e) => setNickname(e.target.value)}
                onBlur={() => {
                  if (nicknameCancelled.current) {
                    nicknameCancelled.current = false;
                    setNickname(lanecho.self?.name ?? "");
                    return;
                  }
                  commitNickname();
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                  if (e.key === "Escape") {
                    nicknameCancelled.current = true;
                    e.currentTarget.blur();
                  }
                }}
                placeholder={t.settings.nicknamePlaceholder}
                className="w-full rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60"
              />

              <div className="gauge-label mt-4 mb-1">{t.settings.language}</div>
              <div className="flex gap-1.5">
                {LANGS.map(([value, label]) => (
                  <SegButton
                    key={value}
                    active={lang === value}
                    onClick={() => {
                      if (value === lang) return;
                      setLang(value);
                      void patchSettings({ language: value });
                    }}
                  >
                    {label}
                  </SegButton>
                ))}
              </div>

              <div className="gauge-label mt-4 mb-1">{t.settings.theme}</div>
              <div className="flex gap-1.5">
                {(
                  [
                    ["system", t.settings.themeSystem],
                    ["light", t.settings.themeLight],
                    ["dark", t.settings.themeDark],
                  ] as [ThemePref, string][]
                ).map(([value, label]) => (
                  <SegButton key={value} active={pref === value} onClick={() => setPref(value)}>
                    {label}
                  </SegButton>
                ))}
              </div>

              <div className="gauge-label mt-4 mb-1">{t.settings.previewDelay}</div>
              <div className="flex items-center gap-2">
                {/* Capped at 5s: anything longer amounts to "never shows",
                    and the user will assume the feature is broken */}
                <NumberField
                  value={settings?.previewDelayMs ?? 150}
                  min={0}
                  max={5000}
                  step={50}
                  onCommit={(v) => patchSettings({ previewDelayMs: v })}
                  className="w-32"
                />
                <span className="text-[11px] text-mist">{t.settings.previewDelayHint}</span>
              </div>

              <ToggleRow
                label={t.settings.autostart}
                checked={settings?.autostart ?? false}
                onChange={(v) => patchSettings({ autostart: v })}
              />
            </div>
          )}

          {/* Devices tab: discovered peers + pair / unpair (the interaction
              lives inside the component; the list also holds paired but
              offline devices, greyed out) */}
          {tab === "devices" && (
            <DeviceList devices={lanecho.devices} onChanged={lanecho.refreshDevices} />
          )}

          {/* Sync tab: master switch / sync notification / pause recording /
              port */}
          {tab === "sync" && (
            <>
              <div className="rounded-xl border border-line bg-panel px-4 pt-3 pb-3">
                <div className="gauge-label mb-1">{t.sync.mode}</div>
                <div className="flex flex-wrap gap-x-5 gap-y-2 py-1">
                  {(
                    [
                      ["off", t.sync.modeOff],
                      ["both", t.sync.modeBoth],
                      ["send", t.sync.modeSend],
                      ["receive", t.sync.modeReceive],
                    ] as [string, string][]
                  ).map(([mode, label]) => (
                    <RadioOption
                      key={mode}
                      checked={(settings?.syncMode ?? "both") === mode}
                      label={label}
                      onSelect={() =>
                        patchSettings({ syncMode: mode, syncEnabled: mode !== "off" })
                      }
                    />
                  ))}
                </div>
                <div className="mt-1.5 text-[11px] text-mist">
                  {{
                    off: t.sync.modeHintOff,
                    both: t.sync.modeHintBoth,
                    send: t.sync.modeHintSend,
                    receive: t.sync.modeHintReceive,
                  }[settings?.syncMode ?? "both"] ?? t.sync.modeHintBoth}
                </div>

                <div className="gauge-label mt-4 mb-1">{t.sync.types}</div>
                <div className="flex flex-wrap gap-x-5 gap-y-2 py-1">
                  {(
                    [
                      ["syncText", t.sync.typeText],
                      ["syncImages", t.sync.typeImages],
                      ["syncFiles", t.sync.typeFiles],
                    ] as [keyof Settings, string][]
                  ).map(([key, label]) => (
                    <CheckOption
                      key={key}
                      checked={Boolean(settings?.[key])}
                      label={label}
                      onToggle={() => patchSettings({ [key]: !settings?.[key] })}
                    />
                  ))}
                </div>
                <div className="mt-1.5 text-[11px] text-mist">{t.sync.typesHint}</div>

                {/* Only meaningful while files are synced: greyed out
                    otherwise, the value is kept */}
                <div className={`transition-opacity ${settings?.syncFiles ? "" : "opacity-45"}`}>
                  <div className="gauge-label mt-4 mb-1">{t.sync.fileLimit}</div>
                  <div className="flex items-center gap-2">
                    <NumberField
                      value={settings?.maxSyncFileMb ?? 32}
                      min={1}
                      max={512}
                      disabled={!settings?.syncFiles}
                      onCommit={(v) => patchSettings({ maxSyncFileMb: v })}
                      className="w-24"
                    />
                    <span className="text-[11px] text-mist">{t.sync.fileLimitHint}</span>
                  </div>
                </div>

                <div className="mt-3 truncate text-[11px] text-faint">
                  {lanecho.lastSync
                    ? `${t.sync.lastFrom(lanecho.lastSync.fromName)} · ${lanecho.lastSync.preview}`
                    : t.sync.idle}
                </div>
              </div>

              <div className="mt-5 rounded-xl border border-line bg-panel px-4 pt-1 pb-3">
                <ToggleRow
                  label={t.settings.notifyOnSync}
                  hint={t.settings.notifyOnSyncHint}
                  checked={settings?.notifyOnSync ?? true}
                  onChange={(v) => patchSettings({ notifyOnSync: v })}
                />
                {/* Pause recording: a runtime cut-off switch like the sync
                    toggle (session-scoped, never persisted) */}
                <ToggleRow
                  label={t.historySettings.incognito}
                  hint={t.historySettings.incognitoHint}
                  checked={incognito}
                  onChange={(v) => {
                    setIncognito(v);
                    api.setIncognito(v).catch(console.error);
                  }}
                />

                <div className="gauge-label mt-4 mb-1">{t.settings.port}</div>
                <div className="flex items-center gap-2">
                  <NumberField
                    value={settings?.tcpPort ?? 0}
                    min={0}
                    max={65535}
                    onCommit={(v) => patchSettings({ tcpPort: v })}
                    className="w-32"
                  />
                  <span className="text-[11px] text-mist">{t.settings.portHint}</span>
                </div>
              </div>
            </>
          )}

          {/* Storage tab: capacity and what gets recorded */}
          {tab === "storage" && (
            <div className="rounded-xl border border-line bg-panel px-4 py-3">
              <div className="gauge-label mb-1 flex items-center justify-between">
                <span>{t.historySettings.maxEntries}</span>
                <span className="normal-case">{t.historySettings.usage(formatBytes(usage))}</span>
              </div>
              {/* Same range as the native Stepper (50...1000): the index is
                  rewritten whole on every copy, so a large cap costs on
                  every copy (the backend itself does not clamp) */}
              <NumberField
                value={settings?.historyMaxEntries ?? 200}
                min={50}
                max={1000}
                step={50}
                onCommit={(v) => patchSettings({ historyMaxEntries: v })}
                className="w-32"
              />

              <div className="gauge-label mt-4 mb-1">{t.historySettings.recordTypes}</div>
              <div className="flex flex-wrap gap-x-5 gap-y-2 py-1">
                {(
                  [
                    ["historyRecordText", t.historySettings.recordText],
                    ["historyRecordImages", t.historySettings.recordImages],
                    ["historyRecordFiles", t.historySettings.recordFiles],
                  ] as [keyof Settings, string][]
                ).map(([key, label]) => (
                  <CheckOption
                    key={key}
                    checked={Boolean(settings?.[key])}
                    label={label}
                    onToggle={() => patchSettings({ [key]: !settings?.[key] })}
                  />
                ))}
              </div>

              <div className="gauge-label mt-4 mb-1">{t.historySettings.sort}</div>
              <div className="flex gap-1.5">
                {(
                  [
                    ["recent", t.historySettings.sortRecent],
                    ["frequent", t.historySettings.sortFrequent],
                  ] as [string, string][]
                ).map(([value, label]) => (
                  <SegButton
                    key={value}
                    active={(settings?.historySort ?? "recent") === value}
                    onClick={() => patchSettings({ historySort: value })}
                  >
                    {label}
                  </SegButton>
                ))}
              </div>
            </div>
          )}

          {/* Ignore tab: the four rule kinds (apps / types / regex / files) */}
          {tab === "ignore" && settings && (
            <IgnorePane
              ignore={settings.ignore}
              onChange={(next) => void patchSettings({ ignore: next })}
              onError={setError}
            />
          )}

          {/* Hotkeys tab: the panel hotkey + direct paste from numbered
              slots */}
          {tab === "hotkeys" && (
            <div className="rounded-xl border border-line bg-panel px-4 py-3">
              <div className="gauge-label mb-1">{t.historySettings.panelHotkey}</div>
              <div className="flex items-center gap-2">
                <HotkeyRecorder
                  value={settings?.panelHotkey ?? ""}
                  onChange={(v) => void patchSettings({ panelHotkey: v }).then(refreshSlotFailures)}
                />
                <span className="text-[11px] text-mist">{t.historySettings.panelHotkeyHint}</span>
              </div>

              <ToggleRow
                label={t.historySettings.slotHotkeys}
                hint={t.historySettings.slotHotkeysHint}
                checked={settings?.slotHotkeys ?? true}
                onChange={(v) => void patchSettings({ slotHotkeys: v }).then(refreshSlotFailures)}
              />
              {(settings?.slotHotkeys ?? true) && (
                <>
                  <div className="gauge-label mt-3 mb-1">{t.historySettings.slotModifier}</div>
                  <div className="flex items-center gap-2">
                    <div className="flex gap-1.5">
                      {SLOT_MODIFIER_OPTIONS.map(({ value, label }) => (
                        <SegButton
                          key={value}
                          active={(settings?.slotModifier ?? "CmdOrCtrl") === value}
                          // The new combination may collide with another app
                          onClick={() =>
                            void patchSettings({ slotModifier: value }).then(refreshSlotFailures)
                          }
                        >
                          {label}
                        </SegButton>
                      ))}
                    </div>
                    <span className="text-[11px] text-mist">
                      {t.historySettings.slotModifierHint}
                    </span>
                  </div>
                </>
              )}
              {(settings?.slotHotkeys ?? true) && slotFailures.length > 0 && (
                <div className="mt-1 text-[11px] text-alert">
                  {t.historySettings.slotConflict(
                    slotFailures
                      .map((n) => `${slotModLabel(settings?.slotModifier ?? "CmdOrCtrl")}${n}`)
                      .join("/"),
                  )}
                </div>
              )}

              {/* Auto-paste: hidden outright where the platform cannot
                  synthesize the keystroke (Linux), rather than shown as a
                  toggle that silently does nothing */}
              {autoPaste?.supported && (
                <>
                  <ToggleRow
                    label={t.historySettings.autoPaste}
                    hint={t.historySettings.autoPasteHint}
                    checked={settings?.autoPaste ?? false}
                    onChange={(v) => {
                      patchSettings({ autoPaste: v });
                      // Switching it on has to ask for the permission right
                      // here: macOS drops the synthesized key silently
                      // without it, so the toggle would just look broken
                      if (v && !autoPaste.permitted) {
                        api.requestAutoPastePermission().then(setAutoPaste).catch(console.error);
                      }
                    }}
                  />
                  {(settings?.autoPaste ?? false) && !autoPaste.permitted && (
                    <div className="mt-1 flex items-start gap-2 text-[11px] text-alert">
                      <span>{t.historySettings.autoPastePermission}</span>
                      <button
                        onClick={() => {
                          api.requestAutoPastePermission().then(setAutoPaste).catch(console.error);
                        }}
                        className="shrink-0 cursor-pointer text-sonar/80 transition-colors hover:text-sonar"
                      >
                        {t.historySettings.autoPasteGrant}
                      </button>
                    </div>
                  )}
                </>
              )}
            </div>
          )}

          {/* Failure of the last change, on whichever tab it happened */}
          {error && <div className="mt-3 px-1 text-xs text-alert">{error}</div>}
        </div>
      </main>

      {/* Local device info (pinned footer, so comparing fingerprints by eye
          while pairing needs no tab switch) */}
      <footer className="flex shrink-0 items-center gap-2 border-t border-line px-6 py-2 text-[11px] text-faint">
        <span>{t.settings.fingerprint}</span>
        <span className="font-gauge">
          {lanecho.self ? `${lanecho.self.fingerprint.slice(0, 16)}…` : "—"}
        </span>
        <button
          onClick={copyFingerprint}
          className="cursor-pointer text-sonar/80 transition-colors hover:text-sonar"
        >
          {copied ? t.settings.copied : t.settings.copy}
        </button>
        <span className="font-gauge ml-auto">
          {lanecho.self && t.settings.port_self(lanecho.self.port)}
        </span>
      </footer>

      {/* Pairing request dialog (one at a time from the queue; key forces a
          remount so no state carries over) */}
      {lanecho.pairRequests[0] && (
        <PairRequestModal
          key={lanecho.pairRequests[0].fingerprint}
          peer={lanecho.pairRequests[0]}
          onRespond={lanecho.respondPair}
        />
      )}
    </div>
  );
}

/** Format a byte count (KB and MB are enough) */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Numeric field committed on Enter / blur, clamped to [min, max]. The draft
 *  is free text while typing: clearing the field leaves it empty instead of
 *  snapping to a fallback digit the next keystroke would append to. An empty
 *  or unreadable draft reverts; Esc reverts without committing */
function NumberField({
  value,
  min,
  max,
  step,
  disabled,
  onCommit,
  className,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  disabled?: boolean;
  onCommit: (value: number) => void;
  className: string;
}) {
  const [draft, setDraft] = useState(String(value));
  const focused = useRef(false);
  const cancelled = useRef(false);
  // Follow the stored value unless the user is typing
  useEffect(() => {
    if (!focused.current) setDraft(String(value));
  }, [value]);

  const commit = () => {
    const parsed = Math.round(Number(draft));
    const next =
      draft.trim() === "" || !Number.isFinite(parsed)
        ? value
        : Math.min(max, Math.max(min, parsed));
    setDraft(String(next));
    if (next !== value) onCommit(next);
  };

  return (
    <input
      type="number"
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onFocus={() => {
        focused.current = true;
      }}
      onBlur={() => {
        focused.current = false;
        if (cancelled.current) {
          cancelled.current = false;
          setDraft(String(value));
          return;
        }
        commit();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") {
          cancelled.current = true;
          e.currentTarget.blur();
        }
      }}
      className={`font-gauge rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60 disabled:cursor-not-allowed ${className}`}
    />
  );
}

/** Radio option: dot + label (for inline radio groups such as the sync
 *  direction policy) */
function RadioOption({
  checked,
  label,
  onSelect,
}: {
  checked: boolean;
  label: string;
  onSelect: () => void;
}) {
  return (
    <button
      onClick={onSelect}
      className="flex cursor-pointer items-center gap-2 text-xs text-fog"
    >
      <span
        className={`grid size-4 place-items-center rounded-full border transition-colors ${
          checked ? "border-sonar bg-sonar" : "border-line-2 bg-abyss/40"
        }`}
      >
        {checked && <span className="size-1.5 rounded-full bg-white" />}
      </span>
      <span className={checked ? "" : "text-fog/75"}>{label}</span>
    </button>
  );
}

/** Checkbox option: box + label (for inline groups such as synced types and
 *  recorded types) */
function CheckOption({
  checked,
  label,
  onToggle,
}: {
  checked: boolean;
  label: string;
  onToggle: () => void;
}) {
  return (
    <button
      onClick={onToggle}
      className="flex cursor-pointer items-center gap-2 text-xs text-fog"
    >
      <span
        className={`grid size-4 place-items-center rounded border transition-colors ${
          checked ? "border-sonar bg-sonar" : "border-line-2 bg-abyss/40"
        }`}
      >
        {checked && (
          <svg width="10" height="10" viewBox="0 0 12 12" fill="none">
            <path
              d="M2.5 6.2 5 8.7l4.5-5.4"
              stroke="white"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        )}
      </span>
      <span className={checked ? "" : "text-fog/75"}>{label}</span>
    </button>
  );
}

/** Segmented control button (language / theme) */
function SegButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`cursor-pointer rounded-md border px-3 py-1.5 text-xs transition-colors ${
        active
          ? "border-sonar/60 bg-sonar/15 text-sonar"
          : "border-line-2 text-fog/70 hover:border-mist hover:text-fog"
      }`}
    >
      {children}
    </button>
  );
}

/** Three-state theme icon: monitor (system) / sun (light) / moon (dark) */
function ThemeIcon({ pref }: { pref: ThemePref }) {
  if (pref === "light") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
      </svg>
    );
  }
  if (pref === "dark") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
      </svg>
    );
  }
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  );
}
