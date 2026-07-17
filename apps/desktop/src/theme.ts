// 主题三态(system / light / dark)hook: deskmate App.tsx 的成熟模式抽取
//
// - 偏好存 localStorage("lanecho-theme"); 仅 light/dark 视为显式值, 其余按 system
// - "跟随系统"经 matchMedia 常驻监听, 系统切换实时生效
// - 原生窗口 chrome 经 setTheme 同步; system 时传 null 交还系统
//   (Tauri setTheme 只接受 "light" | "dark" | null)

import { useCallback, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** 主题偏好三态 */
export type ThemePref = "system" | "light" | "dark";

const THEME_KEY = "lanecho-theme";

/** 系统当前明暗(判 light, 无 matchMedia 环境兜底暗色) */
function systemTheme(): "light" | "dark" {
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** 读取持久化偏好 */
function storedPref(): ThemePref {
  const saved = localStorage.getItem(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : "system";
}

/** 主题状态与三态循环切换(system → light → dark → system) */
export function useTheme() {
  const [pref, setPref] = useState<ThemePref>(storedPref);
  const [sysTheme, setSysTheme] = useState<"light" | "dark">(systemTheme);

  // 常驻监听系统明暗: "跟随系统"时切换实时生效
  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-color-scheme: light)");
    if (!mq) return;
    const onChange = () => setSysTheme(systemTheme());
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  // 跨窗口同步: 历史浮窗是常驻隐藏文档, 主窗切主题写 localStorage 时
  // 经 storage 事件跟随更新(同源多窗共享 localStorage)
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
    // 原生窗口 chrome 同步; system 传 null 交还系统(避免标题栏滞留旧主题)
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
