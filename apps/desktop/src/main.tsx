import React, { Suspense, lazy } from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { I18nProvider } from "./i18n";
import "./index.css";

// Window components are code-split: with all four windows in one bundle every
// WebView has to parse the code of every window; lazy loading leaves each
// window with its own chunk plus the shared vendor chunk.
// The fallback is always null — floating window documents have to stay
// transparent, and any placeholder shows up as a flashed frame
const App = lazy(() => import("./App"));
const HistoryPanel = lazy(() =>
  import("./components/HistoryPanel").then((m) => ({ default: m.HistoryPanel })),
);
const PreviewCard = lazy(() =>
  import("./components/PreviewCard").then((m) => ({ default: m.PreviewCard })),
);
const AboutWindow = lazy(() =>
  import("./components/AboutWindow").then((m) => ({ default: m.AboutWindow })),
);

// Route by window label: panel = the floating history panel, preview = the
// preview card, about = the about window, anything else = the main window.
// In a plain browser (no Tauri) use ?panel=1 / ?preview=1 / ?about=1 instead.
const label = "__TAURI_INTERNALS__" in window
  ? getCurrentWindow().label
  : new URLSearchParams(window.location.search).has("preview")
    ? "preview"
    : new URLSearchParams(window.location.search).has("panel")
      ? "panel"
      : new URLSearchParams(window.location.search).has("about")
        ? "about"
        : "main";
const isPanel = label === "panel";
const isPreview = label === "preview";
const isAbout = label === "about";

// Floating window documents (panel / preview card): the rounded root
// container paints the background and the document itself stays transparent —
// the four corners outside the radius show the desktop through, which is what
// gives a borderless window its rounded shape (index.css [data-floating])
if (isPanel || isPreview) {
  document.documentElement.dataset.floating = "1";
  document.documentElement.style.background = "transparent";
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <Suspense fallback={null}>
        {isPreview ? (
          <PreviewCard />
        ) : isPanel ? (
          <HistoryPanel />
        ) : isAbout ? (
          <AboutWindow />
        ) : (
          <App />
        )}
      </Suspense>
    </I18nProvider>
  </React.StrictMode>,
);
