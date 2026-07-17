// 历史浮窗面板(方案 14.5): 搜索 + 序号槽位 + 键盘全可达
//
// 独立 WebView 窗口(label = "panel", 无边框失焦即隐), 由 main.tsx 按
// 窗口 label 路由到本组件。选中条目 = 还原写入剪贴板并隐藏面板;
// 该写入视同用户复制(方案 6.4), watcher 会正常广播与计数。

import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api";
import { EVENTS } from "../events";
import { formatError, useI18n } from "../i18n";
import { useTheme } from "../theme";
import type { HistoryEntryDto } from "../types";

/** 是否在 Tauri 运行时内 */
const hasTauri = "__TAURI_INTERNALS__" in window;

/** 历史浮窗面板 */
export function HistoryPanel() {
  const { t } = useI18n();
  // 面板是独立 WebView 文档, 需各自应用主题(localStorage 键共享)
  useTheme();
  const [entries, setEntries] = useState<HistoryEntryDto[]>([]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const reload = useCallback(async () => {
    if (!hasTauri) return;
    try {
      const settings = await api.getSettings();
      setEntries(await api.listHistory(settings.historySort));
    } catch (e) {
      console.error(e);
    }
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
    add(listen(EVENTS.HISTORY_CHANGED, reload));
    // 每次唤起(窗口获焦)重置状态: 刷新列表、清搜索、聚焦输入框
    add(
      getCurrentWindow().listen("tauri://focus", () => {
        reload();
        setQuery("");
        setSelected(0);
        setError("");
        inputRef.current?.focus();
      }),
    );
    return () => {
      alive = false;
      unsubs.forEach((unsub) => unsub());
    };
  }, [reload]);

  const lowered = query.toLowerCase();
  const filtered = query
    ? entries.filter(
        (e) =>
          e.preview.toLowerCase().includes(lowered) ||
          (e.text?.toLowerCase().includes(lowered) ?? false),
      )
    : entries;
  // 上下界都要夹取: 空列表按方向键可使 selected 变 -1
  const highlight = Math.max(0, Math.min(selected, filtered.length - 1));

  /** 选中条目: 写剪贴板并隐藏面板 */
  const choose = async (entry: HistoryEntryDto) => {
    try {
      await api.copyHistoryEntry(entry.id);
      setError("");
      await getCurrentWindow().hide();
    } catch (e) {
      setError(formatError(e));
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      void getCurrentWindow().hide();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((s) => Math.min(s + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter" && filtered[highlight]) {
      void choose(filtered[highlight]);
    }
  };

  return (
    <div
      className="flex h-screen flex-col overflow-hidden border border-line-2 bg-panel"
      onKeyDown={onKeyDown}
    >
      {/* 搜索框 */}
      <div className="shrink-0 border-b border-line px-3 py-2">
        <input
          ref={inputRef}
          autoFocus
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setSelected(0);
          }}
          placeholder={t.history.searchPlaceholder}
          className="w-full rounded-md border border-line-2 bg-abyss/60 px-3 py-1.5 text-sm text-fog outline-none focus:border-sonar/60"
        />
      </div>

      {/* 条目列表 */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {filtered.length === 0 ? (
          <div className="px-4 py-8 text-center text-xs text-mist">
            {entries.length === 0 ? t.history.empty : t.history.noMatch}
          </div>
        ) : (
          filtered.map((entry, index) => (
            <div
              key={entry.id}
              onClick={() => void choose(entry)}
              onMouseEnter={() => setSelected(index)}
              // 高亮行随键盘导航滚入视野(block: nearest 幂等, 回调 ref 重复调用无害)
              ref={
                index === highlight
                  ? (el) => el?.scrollIntoView({ block: "nearest" })
                  : undefined
              }
              className={`group flex cursor-pointer items-center gap-2 border-b border-line/40 px-3 py-2 last:border-b-0 ${
                index === highlight ? "bg-sonar/10" : ""
              }`}
            >
              {/* 序号角标: 仅无搜索词时显示 —— Alt+N 槽位取的是未过滤
                  全量排序表第 N 条, 过滤中显示角标会误导 */}
              <span className="font-gauge w-4 shrink-0 text-center text-[10px] text-faint">
                {!query && index < 6 ? index + 1 : ""}
              </span>
              <KindIcon kind={entry.kind} />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] text-fog">
                  {entry.kind === "image"
                    ? t.history.imageLabel(entry.preview)
                    : entry.preview || "␣"}
                </div>
                <div className="font-gauge flex items-center gap-2 text-[10px] text-mist">
                  {entry.copyCount > 1 && <span>×{entry.copyCount}</span>}
                  {entry.origin && <span>{t.history.fromDevice(entry.origin)}</span>}
                </div>
              </div>
              {entry.pinned && <PinBadge />}
              {/* 悬停操作: 固定 / 删除 */}
              <div className="hidden shrink-0 items-center gap-1 group-hover:flex">
                <button
                  title={entry.pinned ? t.history.unpin : t.history.pin}
                  onClick={(e) => {
                    e.stopPropagation();
                    void api.pinHistoryEntry(entry.id, !entry.pinned);
                  }}
                  className="cursor-pointer rounded p-1 text-mist hover:text-sonar"
                >
                  <PinIcon />
                </button>
                <button
                  title={t.history.delete}
                  onClick={(e) => {
                    e.stopPropagation();
                    void api.deleteHistoryEntry(entry.id);
                  }}
                  className="cursor-pointer rounded p-1 text-mist hover:text-alert"
                >
                  ✕
                </button>
              </div>
            </div>
          ))
        )}
      </div>

      {/* 底部状态条 */}
      <div className="flex shrink-0 items-center justify-between border-t border-line px-3 py-1.5">
        <span className="font-gauge text-[10px] text-mist">
          {error ? (
            <span className="text-alert">{error}</span>
          ) : (
            t.history.count(filtered.length)
          )}
        </span>
        <ClearHistoryButton />
      </div>
    </div>
  );
}

/** 清空按钮: 两段确认(点一次进入确认态, 3s 未确认自动复位) */
function ClearHistoryButton() {
  const { t } = useI18n();
  const [arming, setArming] = useState(false);
  useEffect(() => {
    if (!arming) return;
    const timer = setTimeout(() => setArming(false), 3000);
    return () => clearTimeout(timer);
  }, [arming]);
  return (
    <button
      onClick={() => {
        if (arming) {
          void api.clearHistory();
          setArming(false);
        } else {
          setArming(true);
        }
      }}
      className={`cursor-pointer text-[10px] transition-colors ${
        arming ? "text-alert" : "text-mist hover:text-fog"
      }`}
    >
      {arming ? t.history.clearConfirm : t.history.clear}
    </button>
  );
}

/** 类型小图标 */
function KindIcon({ kind }: { kind: string }) {
  const cls = "size-3.5 shrink-0 text-mist";
  if (kind === "image") {
    return (
      <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <circle cx="8.5" cy="8.5" r="1.5" fill="currentColor" stroke="none" />
        <path d="m21 15-5-5L5 21" />
      </svg>
    );
  }
  if (kind === "files") {
    return (
      <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
        <path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />
        <path d="M13 2v7h7" />
      </svg>
    );
  }
  return (
    <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M4 7h16M4 12h10M4 17h13" />
    </svg>
  );
}

/** 已固定标记 */
function PinBadge() {
  return <span className="shrink-0 text-[10px] text-sonar">●</span>;
}

/** 固定操作图标 */
function PinIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M12 17v5M9 3h6l1 7 2 2H6l2-2z" />
    </svg>
  );
}
