import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { HistoryPanel } from "./components/HistoryPanel";
import { I18nProvider } from "./i18n";
import "./index.css";

// 按窗口 label 路由: panel = 历史浮窗, 其余 = 主窗口。
// 浏览器预览(无 Tauri)用 ?panel=1 查看面板视图。
const isPanel =
  "__TAURI_INTERNALS__" in window
    ? getCurrentWindow().label === "panel"
    : new URLSearchParams(window.location.search).has("panel");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>{isPanel ? <HistoryPanel /> : <App />}</I18nProvider>
  </React.StrictMode>,
);
