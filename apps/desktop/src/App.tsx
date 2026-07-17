// lanecho 主界面: 单页布局(同步开关 / 设备列表 / 设置 / 本机信息)
// 托盘常驻应用的"控制面板", 无雷达无拖拽 —— 刻意极简(方案第 8 节)

import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { EVENTS } from "./events";
import { PairRequestModal } from "./components/PairRequestModal";
import { Button, ToggleRow } from "./components/ModalShell";
import { useLanecho } from "./hooks/useLanecho";
import { formatError, useI18n, type Lang } from "./i18n";
import { useTheme, type ThemePref } from "./theme";
import type { DeviceDto, Settings } from "./types";

/** 是否在 Tauri 运行时内 */
const hasTauri = "__TAURI_INTERNALS__" in window;

/** 语言选项 */
const LANGS: [Lang, string][] = [
  ["zh", "中文"],
  ["en", "English"],
];

export default function App() {
  const { lang, t, setLang } = useI18n();
  const { pref, cycle, setPref } = useTheme();

  const [settings, setSettings] = useState<Settings | null>(null);
  const lanecho = useLanecho({
    // 托盘切换同步开关 → 设置区即时回显
    onSyncState: (enabled) =>
      setSettings((s) => (s ? { ...s, syncEnabled: enabled } : s)),
  });

  // 表单态(保存时提交)
  const [nickname, setNickname] = useState("");
  const [portInput, setPortInput] = useState(0);
  const [langChoice, setLangChoice] = useState<Lang>(lang);
  const [tip, setTip] = useState("");
  const [pairError, setPairError] = useState("");
  const [pairingWith, setPairingWith] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [incognito, setIncognito] = useState(false);
  const [usage, setUsage] = useState(0);
  const [hotkeyInput, setHotkeyInput] = useState("");
  const [maxEntriesInput, setMaxEntriesInput] = useState(200);

  // 初始加载设置 + 历史占用 + 无痕状态(托盘切换经事件回显)
  useEffect(() => {
    if (!hasTauri) return;
    api
      .getSettings()
      .then((s) => {
        setSettings(s);
        setPortInput(s.tcpPort);
        setHotkeyInput(s.panelHotkey);
        setMaxEntriesInput(s.historyMaxEntries);
      })
      .catch(console.error);
    api.getIncognito().then(setIncognito).catch(console.error);
    api.historyUsage().then(setUsage).catch(console.error);
    let alive = true;
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
    add(
      listen(EVENTS.HISTORY_CHANGED, () => {
        api
          .historyUsage()
          .then((v) => alive && setUsage(v))
          .catch(console.error);
      }),
    );
    return () => {
      alive = false;
      unsubs.forEach((u) => u());
    };
  }, []);
  // 本机名仅在首次加载(或保存后主动请求)时回填, 不覆盖正在输入的值
  const nicknameSynced = useRef(false);
  useEffect(() => {
    if (lanecho.self && !nicknameSynced.current) {
      nicknameSynced.current = true;
      setNickname(lanecho.self.name);
    }
  }, [lanecho.self]);
  useEffect(() => setLangChoice(lang), [lang]);

  /** 开关类设置: 即改即存(三个开关统一语义, 与托盘行为一致) */
  const patchSettings = (patch: Partial<Settings>) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    api.saveSettings(next).catch((e) => setTip(formatError(e)));
  };

  /** 保存设置(名称走独立命令, 其余整体提交) */
  const save = async () => {
    if (!settings) return;
    setTip("");
    try {
      const trimmed = nickname.trim();
      if (lanecho.self && trimmed !== lanecho.self.name) {
        await api.setDisplayName(trimmed || null);
        // 允许下一次 self 刷新回填(清空恢复 hostname 的场景要显示实际新名)
        nicknameSynced.current = false;
        lanecho.refreshSelf();
      }
      const next: Settings = {
        ...settings,
        tcpPort: portInput,
        language: langChoice,
        panelHotkey: hotkeyInput.trim(),
        historyMaxEntries: Math.max(1, maxEntriesInput),
      };
      await api.saveSettings(next);
      setSettings(next);
      if (langChoice !== lang) setLang(langChoice);
      setTip(t.settings.saved);
      setTimeout(() => setTip(""), 2500);
    } catch (e) {
      setTip(formatError(e));
    }
  };

  /** 发起配对(等待对端确认, 按钮呈等待态) */
  const pair = async (device: DeviceDto) => {
    setPairError("");
    setPairingWith(device.fingerprint);
    try {
      await api.pairDevice(device.fingerprint);
      lanecho.refreshDevices();
    } catch (e) {
      setPairError(formatError(e));
    } finally {
      setPairingWith(null);
    }
  };

  /** 解除配对 */
  const unpair = (device: DeviceDto) => {
    setPairError("");
    api
      .unpairDevice(device.fingerprint)
      .then(() => lanecho.refreshDevices())
      .catch((e) => setPairError(formatError(e)));
  };

  /** 复制本机指纹 */
  const copyFingerprint = () => {
    if (!lanecho.self) return;
    navigator.clipboard.writeText(lanecho.self.fingerprint).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };

  const themeTitle =
    pref === "system" ? t.header.toLight : pref === "light" ? t.header.toDark : t.header.toSystem;

  return (
    <div className="flex h-full flex-col">
      {/* 顶栏 */}
      <header className="flex shrink-0 items-center justify-between border-b border-line px-6 py-3">
        <div className="flex items-center gap-3">
          <img src="/logo.svg" className="size-8 rounded-lg" alt="" />
          <div>
            <div className="text-sm font-medium text-fog">lanecho</div>
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

      <main className="min-h-0 flex-1 overflow-y-auto px-6 py-4">
        {/* 同步总开关 */}
        <div className="rounded-xl border border-line bg-panel px-4 pt-1 pb-3">
          <ToggleRow
            label={t.sync.toggle}
            hint={t.sync.toggleHint}
            checked={settings?.syncEnabled ?? true}
            onChange={(v) => patchSettings({ syncEnabled: v })}
          />
          <div className="mt-2 truncate text-[11px] text-faint">
            {lanecho.lastSync
              ? `${t.sync.lastFrom(lanecho.lastSync.fromName)} · ${lanecho.lastSync.preview}`
              : t.sync.idle}
          </div>
        </div>

        {/* 设备列表 */}
        <div className="gauge-label mt-5 mb-2">{t.devices.section}</div>
        <div className="rounded-xl border border-line bg-panel">
          {lanecho.devices.length === 0 ? (
            <div className="px-4 py-6 text-center text-xs text-mist">{t.devices.empty}</div>
          ) : (
            lanecho.devices.map((device) => (
              <div
                key={device.fingerprint}
                className="flex items-center gap-3 border-b border-line/40 px-4 py-2.5 last:border-b-0"
              >
                <span
                  className={`size-2 shrink-0 rounded-full ${
                    device.online ? "anim-breathe bg-live" : "bg-faint"
                  }`}
                  title={device.online ? t.devices.online : t.devices.offline}
                />
                <div className="min-w-0 flex-1">
                  <div
                    className={`truncate text-sm ${device.online ? "text-fog" : "text-mist"}`}
                  >
                    {device.name}
                  </div>
                  <div className="font-gauge text-[10px] text-mist">
                    {device.fingerprint.slice(0, 8)}
                    {device.platform ? ` · ${device.platform}` : ""}
                    {device.osVersion ? ` · ${device.osVersion}` : ""}
                  </div>
                </div>
                {device.paired && (
                  <span className="shrink-0 rounded bg-chip px-1.5 py-0.5 text-[10px] text-sonar">
                    {t.devices.paired}
                  </span>
                )}
                {device.paired ? (
                  <Button variant="danger" onClick={() => unpair(device)}>
                    {t.devices.unpair}
                  </Button>
                ) : (
                  device.online && (
                    <Button
                      variant="primary"
                      disabled={pairingWith !== null}
                      onClick={() => pair(device)}
                    >
                      {pairingWith === device.fingerprint ? t.devices.pairing : t.devices.pair}
                    </Button>
                  )
                )}
              </div>
            ))
          )}
        </div>
        {pairError && <div className="mt-2 text-xs text-alert">{pairError}</div>}

        {/* 设置 */}
        <div className="gauge-label mt-5 mb-2">{t.settings.section}</div>
        <div className="rounded-xl border border-line bg-panel px-4 py-3">
          <div className="gauge-label mb-1">{t.settings.nickname}</div>
          <input
            value={nickname}
            onChange={(e) => setNickname(e.target.value)}
            placeholder={t.settings.nicknamePlaceholder}
            className="w-full rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60"
          />

          <div className="gauge-label mt-4 mb-1">{t.settings.port}</div>
          <div className="flex items-center gap-2">
            <input
              type="number"
              min={0}
              max={65535}
              value={portInput}
              onChange={(e) => setPortInput(Number(e.target.value) || 0)}
              className="font-gauge w-32 rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60"
            />
            <span className="text-[11px] text-mist">{t.settings.portHint}</span>
          </div>

          <div className="gauge-label mt-4 mb-1">{t.settings.language}</div>
          <div className="flex gap-1.5">
            {LANGS.map(([value, label]) => (
              <SegButton
                key={value}
                active={langChoice === value}
                onClick={() => setLangChoice(value)}
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

          <ToggleRow
            label={t.settings.autostart}
            checked={settings?.autostart ?? false}
            onChange={(v) => patchSettings({ autostart: v })}
          />
          <ToggleRow
            label={t.settings.notifyOnSync}
            hint={t.settings.notifyOnSyncHint}
            checked={settings?.notifyOnSync ?? true}
            onChange={(v) => patchSettings({ notifyOnSync: v })}
          />

          {/* 历史分区(方案 14.7) */}
          <div className="gauge-label mt-5 mb-1 border-t border-line pt-4">
            {t.historySettings.section}
            <span className="ml-2 normal-case">
              {t.historySettings.usage(formatBytes(usage))}
            </span>
          </div>

          <div className="gauge-label mt-3 mb-1">{t.historySettings.maxEntries}</div>
          <input
            type="number"
            min={1}
            max={10000}
            value={maxEntriesInput}
            onChange={(e) => setMaxEntriesInput(Number(e.target.value) || 1)}
            className="font-gauge w-32 rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60"
          />

          <div className="gauge-label mt-4 mb-1">{t.historySettings.recordTypes}</div>
          <div className="flex gap-1.5">
            {(
              [
                ["historyRecordText", t.historySettings.recordText],
                ["historyRecordImages", t.historySettings.recordImages],
                ["historyRecordFiles", t.historySettings.recordFiles],
              ] as [keyof Settings, string][]
            ).map(([key, label]) => (
              <SegButton
                key={key}
                active={Boolean(settings?.[key])}
                onClick={() => patchSettings({ [key]: !settings?.[key] })}
              >
                {label}
              </SegButton>
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

          <div className="gauge-label mt-4 mb-1">{t.historySettings.panelHotkey}</div>
          <div className="flex items-center gap-2">
            <input
              value={hotkeyInput}
              onChange={(e) => setHotkeyInput(e.target.value)}
              placeholder="CmdOrCtrl+Shift+V"
              className="font-gauge w-56 rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60"
            />
            <span className="text-[11px] text-mist">{t.historySettings.panelHotkeyHint}</span>
          </div>

          <ToggleRow
            label={t.historySettings.slotHotkeys}
            hint={t.historySettings.slotHotkeysHint}
            checked={settings?.slotHotkeys ?? true}
            onChange={(v) => patchSettings({ slotHotkeys: v })}
          />
          <ToggleRow
            label={t.historySettings.incognito}
            hint={t.historySettings.incognitoHint}
            checked={incognito}
            onChange={(v) => {
              setIncognito(v);
              api.setIncognito(v).catch(console.error);
            }}
          />

          <div className="mt-4 flex items-center justify-end gap-3 border-t border-line pt-3">
            {tip && <span className="max-w-64 truncate text-xs text-mist">{tip}</span>}
            <Button variant="primary" onClick={save}>
              {t.settings.save}
            </Button>
          </div>
        </div>

        {/* 本机信息 */}
        <div className="mt-4 flex items-center gap-2 pb-2 text-[11px] text-faint">
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
        </div>
      </main>

      {/* 配对请求弹窗(队列逐个; key 强制重挂载防状态残留) */}
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

/** 字节数格式化(KB/MB 两级足够) */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** 分段选择按钮(语言/主题/历史类型) */
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

/** 主题三态图标: monitor(system)/ sun(light)/ moon(dark) */
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
