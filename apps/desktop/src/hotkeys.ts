// Keyboard helpers shared by the settings page and the panel: slot modifier
// labels, the accelerator syntax (display + recording) and the in-panel
// shortcuts. Stored values are Tauri accelerator tokens ("CmdOrCtrl" / "Alt"
// / "Ctrl"); what the user should read is the key it lands on for this
// platform.

/** Whether this frontend runs on macOS (same probe the panel already used) */
export const IS_MAC = navigator.userAgent.includes("Mac");

/** Display label for a slot modifier, to prefix the digit ("⌘" → "⌘1",
 *  "Ctrl+" → "Ctrl+1"). Unknown values fall back to the default modifier,
 *  mirroring the backend normalization. */
export function slotModLabel(modifier: string): string {
  if (IS_MAC) {
    if (modifier === "Alt") return "⌥";
    if (modifier === "Ctrl") return "⌃";
    return "⌘";
  }
  return modifier === "Alt" ? "Alt+" : "Ctrl+";
}

/** The modifier choices offered on this platform (value = stored token).
 *  CmdOrCtrl already means Ctrl outside macOS, so a literal "Ctrl" option
 *  would be a duplicate there and is macOS-only. */
export const SLOT_MODIFIER_OPTIONS: { value: string; label: string }[] = IS_MAC
  ? [
      { value: "CmdOrCtrl", label: "⌘ Command" },
      { value: "Alt", label: "⌥ Option" },
      { value: "Ctrl", label: "⌃ Control" },
    ]
  : [
      { value: "CmdOrCtrl", label: "Ctrl" },
      { value: "Alt", label: "Alt" },
    ];

/** Accelerator string → what the user reads: macOS symbols in the system
 *  order (CmdOrCtrl+Shift+C → ⇧⌘C), elsewhere Ctrl+Shift+C. Same token rules
 *  as the native client's symbolize, so both render one settings.json alike */
export function formatAccelerator(accelerator: string): string {
  let cmd = false;
  let ctrl = false;
  let alt = false;
  let shift = false;
  let superKey = false;
  let key = "";
  for (const raw of accelerator.split("+")) {
    const token = raw.trim();
    switch (token.toLowerCase()) {
      case "cmdorctrl":
      case "cmdorcontrol":
      case "commandorctrl":
      case "commandorcontrol":
        cmd = true;
        break;
      case "cmd":
      case "command":
      case "super":
      case "meta":
        superKey = true;
        break;
      case "ctrl":
      case "control":
        ctrl = true;
        break;
      case "alt":
      case "option":
        alt = true;
        break;
      case "shift":
        shift = true;
        break;
      default:
        key = token.length === 1 ? token.toUpperCase() : token;
    }
  }
  if (IS_MAC) {
    return `${ctrl ? "⌃" : ""}${alt ? "⌥" : ""}${shift ? "⇧" : ""}${cmd || superKey ? "⌘" : ""}${key}`;
  }
  return [cmd || ctrl ? "Ctrl" : "", alt ? "Alt" : "", shift ? "Shift" : "", superKey ? "Win" : "", key]
    .filter(Boolean)
    .join("+");
}

/** Physical key (KeyboardEvent.code) → main-key token of the accelerator
 *  syntax, null when the key cannot anchor a hotkey. Punctuation is spelled
 *  as the literal character, as the native recorder writes it; the
 *  global-hotkey parser accepts both spellings */
function keyToken(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-2])$/.test(code)) return code;
  const named: Record<string, string> = {
    Comma: ",",
    Period: ".",
    Slash: "/",
    Semicolon: ";",
    Quote: "'",
    BracketLeft: "[",
    BracketRight: "]",
    Backslash: "\\",
    Backquote: "`",
    Minus: "-",
    Equal: "=",
    Space: "Space",
    Tab: "Tab",
    Enter: "Enter",
    Backspace: "Backspace",
    Delete: "Delete",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    ArrowUp: "Up",
    ArrowDown: "Down",
  };
  return named[code] ?? null;
}

/** Modifier tokens held in a key event, in the native serializer's order */
function modifierTokens(e: KeyboardEvent): string[] {
  const parts: string[] = [];
  if (IS_MAC ? e.metaKey : e.ctrlKey) parts.push("CmdOrCtrl");
  if (IS_MAC && e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (!IS_MAC && e.metaKey) parts.push("Super");
  return parts;
}

/** A recorded key event → accelerator string, or null to keep waiting (a
 *  lone modifier, an unsupported key, or no real modifier: Shift alone would
 *  turn a capital letter into a system-wide hotkey) */
export function acceleratorFromEvent(e: KeyboardEvent): string | null {
  const key = keyToken(e.code);
  const mods = modifierTokens(e);
  if (!key || mods.filter((m) => m !== "Shift").length === 0) return null;
  return [...mods, key].join("+");
}

/** The modifiers currently held, formatted for the recorder's live echo */
export function heldModifiers(e: KeyboardEvent): string {
  return formatAccelerator(modifierTokens(e).join("+"));
}

/** In-panel commands reachable from the keyboard (same set as the native
 *  panel; ⌘ becomes Ctrl off macOS) */
export type PanelCommand = "delete" | "clear" | "pin" | "settings" | "quit";

/** Hint labels shown next to the footer menu rows and in the row tooltips */
export const PANEL_KEYS: Record<PanelCommand, string> = IS_MAC
  ? { delete: "⌘⌫", clear: "⌥⌘⌫", pin: "⌥P", settings: "⌘,", quit: "⌘Q" }
  : {
      delete: "Ctrl+Backspace",
      clear: "Ctrl+Alt+Backspace",
      pin: "Alt+P",
      settings: "Ctrl+,",
      quit: "Ctrl+Q",
    };

/** Panel key event → command. Letters match on the physical code: ⌥P
 *  reports key "π" on macOS */
export function panelCommand(e: KeyboardEvent): PanelCommand | null {
  const primary = IS_MAC ? e.metaKey : e.ctrlKey;
  // The other platform modifier must be up (⌃ on macOS, Win elsewhere)
  if ((IS_MAC ? e.ctrlKey : e.metaKey) || e.shiftKey) return null;
  if (e.key === "Backspace" && primary) return e.altKey ? "clear" : "delete";
  if (e.code === "KeyP" && e.altKey && !primary) return "pin";
  if (e.code === "Comma" && primary && !e.altKey) return "settings";
  if (e.code === "KeyQ" && primary && !e.altKey) return "quit";
  return null;
}
