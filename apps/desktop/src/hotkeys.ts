// Slot-shortcut modifier helpers, shared by the settings page and the panel
// slot badges. The stored value is a Tauri accelerator token ("CmdOrCtrl" /
// "Alt" / "Ctrl"); what the user should read is the key it lands on for this
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
