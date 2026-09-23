// Hotkey recorder (the native client's pill, ported): click to record, press
// the combination, done. The value is the accelerator string the backend
// registers, written in the same syntax as the native recorder so the shared
// settings.json reads the same in both clients.
//
// A focusable div rather than a <button>: WebKit does not focus buttons on
// click, and the keystrokes have to land on this element. Esc cancels, blur
// cancels, and a bare modifier or Shift-only chord keeps waiting.

import { useRef, useState } from "react";
import { useI18n } from "../i18n";
import { acceleratorFromEvent, formatAccelerator, heldModifiers } from "../hotkeys";

export function HotkeyRecorder({
  value,
  onChange,
}: {
  /** Current accelerator ("" = unbound) */
  value: string;
  /** A recorded or cleared accelerator */
  onChange: (accelerator: string) => void;
}) {
  const { t } = useI18n();
  const [recording, setRecording] = useState(false);
  // Modifiers held so far while recording (live echo before the main key)
  const [held, setHeld] = useState("");
  const boxRef = useRef<HTMLDivElement>(null);

  const start = () => {
    setHeld("");
    setRecording(true);
    boxRef.current?.focus();
  };
  const stop = () => {
    setRecording(false);
    setHeld("");
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!recording) {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        start();
      }
      return;
    }
    // Every key belongs to the recorder while it listens (⌘W, Tab and the
    // like must not act on the window)
    e.preventDefault();
    e.stopPropagation();
    const native = e.nativeEvent;
    if (e.key === "Escape" && !heldModifiers(native)) {
      stop();
      return;
    }
    const accelerator = acceleratorFromEvent(native);
    if (!accelerator) {
      setHeld(heldModifiers(native));
      return;
    }
    stop();
    if (accelerator !== value) onChange(accelerator);
  };

  return (
    <div
      ref={boxRef}
      tabIndex={0}
      role="button"
      aria-pressed={recording}
      onClick={() => (recording ? stop() : start())}
      onKeyDown={onKeyDown}
      onKeyUp={(e) => recording && setHeld(heldModifiers(e.nativeEvent))}
      onBlur={stop}
      className={`relative flex h-8 w-44 shrink-0 cursor-pointer items-center justify-center rounded-md border bg-abyss/60 px-7 text-sm outline-none transition-colors ${
        recording ? "border-sonar text-sonar" : "border-line-2 text-fog hover:border-mist"
      }`}
    >
      <span className={`truncate ${recording || value ? "font-gauge" : "text-mist"}`}>
        {recording ? held || t.historySettings.hotkeyPressKeys : value ? formatAccelerator(value) : t.historySettings.hotkeyUnset}
      </span>
      {value && !recording && (
        <button
          title={t.historySettings.hotkeyClear}
          // mousedown would otherwise move focus and the click would toggle
          // recording on the pill underneath
          onMouseDown={(e) => e.preventDefault()}
          onClick={(e) => {
            e.stopPropagation();
            onChange("");
          }}
          className="absolute right-1.5 grid size-4 cursor-pointer place-items-center rounded-full text-[10px] leading-none text-mist transition-colors hover:bg-line-2 hover:text-fog"
        >
          ✕
        </button>
      )}
    </div>
  );
}
