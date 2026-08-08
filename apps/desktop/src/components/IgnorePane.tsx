// The Ignore settings tab: four rule kinds behind a segmented switch (apps /
// pasteboard types / regex / file patterns), matching the native macOS
// client's editor — a selectable list with a +/− pair (+ opens the system
// picker on the apps pane, an inline entry row on the types/regex panes),
// and the two suppression toggles as one row of checkboxes shared by all
// four panes.

import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import { DEFAULT_IGNORE_TYPES, type IgnoreSettings } from "../types";

/** The four panes */
type Pane = "apps" | "types" | "regex" | "files";

/** Whether this platform can identify a source application (macOS bundle id /
 *  Windows exe name; Linux cannot, so the apps pane shows a notice) */
const APPS_SUPPORTED =
  navigator.userAgent.includes("Mac") || navigator.userAgent.includes("Windows");

export function IgnorePane({
  ignore,
  onChange,
  onError,
}: {
  ignore: IgnoreSettings;
  /** Persist a new ignore object (the caller patches settings) */
  onChange: (next: IgnoreSettings) => void;
  onError: (message: string) => void;
}) {
  const { t } = useI18n();
  const [pane, setPane] = useState<Pane>("apps");
  // Selected row (apps select by id, types/regex by the value)
  const [selection, setSelection] = useState<string | null>(null);
  // Inline entry row (types/regex): + appends it, Enter commits, Esc cancels
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  // File pane editor text, committed on blur (typing straight into settings
  // would save on every keystroke)
  const [fileText, setFileText] = useState(ignore.filePatterns);
  const draftRef = useRef<HTMLInputElement>(null);

  // The panel state resets on pane switch; the file editor follows external
  // changes only while not focused (it is the source of truth while editing)
  useEffect(() => {
    setSelection(null);
    setAdding(false);
    setDraft("");
  }, [pane]);
  useEffect(() => {
    if (document.activeElement?.id !== "ignore-file-editor") {
      setFileText(ignore.filePatterns);
    }
  }, [ignore.filePatterns]);
  useEffect(() => {
    if (adding) draftRef.current?.focus();
  }, [adding]);

  const note = {
    apps: APPS_SUPPORTED ? t.ignore.appsNote : t.ignore.appsUnsupported,
    types: t.ignore.typesNote,
    regex: t.ignore.regexNote,
    files: t.ignore.filesNote,
  }[pane];

  /** The toggle pair of the current pane */
  const toggles: [keyof IgnoreSettings, keyof IgnoreSettings] = {
    apps: ["appsSync", "appsRecord"] as [keyof IgnoreSettings, keyof IgnoreSettings],
    types: ["typesSync", "typesRecord"] as [keyof IgnoreSettings, keyof IgnoreSettings],
    regex: ["regexSync", "regexRecord"] as [keyof IgnoreSettings, keyof IgnoreSettings],
    files: ["filesSync", "filesRecord"] as [keyof IgnoreSettings, keyof IgnoreSettings],
  }[pane];

  const commitDraft = () => {
    const value = draft.trim();
    if (value) {
      if (pane === "types" && !ignore.types.includes(value)) {
        onChange({ ...ignore, types: [...ignore.types, value] });
      } else if (pane === "regex" && !ignore.regexes.includes(value)) {
        onChange({ ...ignore, regexes: [...ignore.regexes, value] });
      }
    }
    setDraft("");
    setAdding(false);
  };

  const add = () => {
    if (pane === "apps") {
      if (!APPS_SUPPORTED) return;
      api
        .pickIgnoredApp()
        .then((app) => {
          if (app && !ignore.apps.some((existing) => existing.id === app.id)) {
            onChange({ ...ignore, apps: [...ignore.apps, app] });
          }
        })
        .catch((e) => onError(formatError(e)));
    } else if (pane === "types" || pane === "regex") {
      if (adding) draftRef.current?.focus();
      else setAdding(true);
    }
  };

  const removeSelected = () => {
    if (!selection) return;
    if (pane === "apps") {
      onChange({ ...ignore, apps: ignore.apps.filter((app) => app.id !== selection) });
    } else if (pane === "types") {
      onChange({ ...ignore, types: ignore.types.filter((item) => item !== selection) });
    } else if (pane === "regex") {
      onChange({ ...ignore, regexes: ignore.regexes.filter((item) => item !== selection) });
    }
    setSelection(null);
  };

  const rows: { id: string; primary: string; secondary?: string }[] =
    pane === "apps"
      ? ignore.apps.map((app) => ({ id: app.id, primary: app.name, secondary: app.id }))
      : pane === "types"
        ? ignore.types.map((item) => ({ id: item, primary: item }))
        : pane === "regex"
          ? ignore.regexes.map((item) => ({ id: item, primary: item }))
          : [];

  return (
    <div className="rounded-xl border border-line bg-panel px-4 py-3">
      {/* Pane switch */}
      <div className="flex gap-1.5">
        {(
          [
            ["apps", t.ignore.paneApps],
            ["types", t.ignore.paneTypes],
            ["regex", t.ignore.paneRegex],
            ["files", t.ignore.paneFiles],
          ] as [Pane, string][]
        ).map(([value, label]) => (
          <button
            key={value}
            onClick={() => setPane(value)}
            className={`cursor-pointer rounded-md px-2.5 py-1 text-xs transition-colors ${
              pane === value
                ? "bg-sonar/15 text-sonar"
                : "text-mist hover:bg-abyss/60 hover:text-fog"
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {/* List / editor area (fixed height so pane switches do not jump) */}
      <div className="mt-2 h-56 overflow-y-auto rounded-md border border-line-2 bg-abyss/40">
        {pane === "files" ? (
          <textarea
            id="ignore-file-editor"
            value={fileText}
            onChange={(e) => setFileText(e.target.value)}
            onBlur={() => {
              if (fileText !== ignore.filePatterns) {
                onChange({ ...ignore, filePatterns: fileText });
              }
            }}
            spellCheck={false}
            className="font-gauge size-full resize-none bg-transparent px-3 py-2 text-sm text-fog outline-none"
          />
        ) : rows.length === 0 && !adding ? (
          <div className="grid h-full place-items-center text-xs text-mist">
            {t.ignore.empty}
          </div>
        ) : (
          <div onClick={() => setSelection(null)}>
            {rows.map((row) => (
              <button
                key={row.id}
                onClick={(e) => {
                  e.stopPropagation();
                  setSelection(row.id);
                }}
                className={`flex w-full cursor-default items-baseline gap-2 border-b border-line/60 px-3 py-1.5 text-left last:border-b-0 ${
                  selection === row.id ? "bg-sonar/20" : ""
                }`}
              >
                <span className="font-gauge truncate text-sm text-fog">{row.primary}</span>
                {row.secondary && (
                  <span className="truncate text-[11px] text-mist">{row.secondary}</span>
                )}
              </button>
            ))}
            {adding && (
              <input
                ref={draftRef}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitDraft();
                  if (e.key === "Escape") {
                    setDraft("");
                    setAdding(false);
                  }
                }}
                onBlur={() => {
                  // Focus moving elsewhere commits what was typed (Escape
                  // already cleared `adding` before the blur lands)
                  if (adding) commitDraft();
                }}
                placeholder={t.ignore.inlinePlaceholder}
                className="font-gauge w-full bg-transparent px-3 py-1.5 text-sm text-fog outline-none placeholder:text-mist/60"
              />
            )}
          </div>
        )}
      </div>

      {/* Bottom bar: the +/− pair (hidden on the file pane) plus reset */}
      {pane !== "files" && (
        <div className="mt-2 flex items-center gap-2">
          <div className="flex overflow-hidden rounded-md border border-line-2">
            <button
              onClick={add}
              disabled={pane === "apps" && !APPS_SUPPORTED}
              className="cursor-pointer px-2.5 py-1 text-sm text-fog transition-colors hover:bg-abyss/60 disabled:cursor-not-allowed disabled:text-mist/50"
            >
              +
            </button>
            <div className="w-px bg-line-2" />
            <button
              onClick={removeSelected}
              disabled={!selection}
              className="cursor-pointer px-2.5 py-1 text-sm text-fog transition-colors hover:bg-abyss/60 disabled:cursor-not-allowed disabled:text-mist/50"
            >
              −
            </button>
          </div>
          <div className="flex-1" />
          {pane === "types" && (
            <button
              onClick={() => onChange({ ...ignore, types: [...DEFAULT_IGNORE_TYPES] })}
              className="cursor-pointer rounded-md border border-line-2 px-2.5 py-1 text-xs text-fog transition-colors hover:bg-abyss/60"
            >
              {t.ignore.reset}
            </button>
          )}
        </div>
      )}

      <div className="mt-1.5 text-[11px] text-mist">{note}</div>

      {/* The suppression pair, shared by all four panes */}
      <div className="mt-3 flex gap-5 border-t border-line pt-3">
        {(
          [
            [toggles[0], t.ignore.suppressSync],
            [toggles[1], t.ignore.suppressRecord],
          ] as [keyof IgnoreSettings, string][]
        ).map(([key, label]) => (
          <button
            key={key}
            onClick={() => onChange({ ...ignore, [key]: !ignore[key] })}
            className="flex cursor-pointer items-center gap-2 text-xs text-fog"
          >
            <span
              className={`grid size-4 place-items-center rounded border transition-colors ${
                ignore[key] ? "border-sonar bg-sonar" : "border-line-2 bg-abyss/40"
              }`}
            >
              {ignore[key] && (
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
            {label}
          </button>
        ))}
      </div>
    </div>
  );
}
