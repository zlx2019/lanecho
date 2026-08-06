// Shared dialog base: the generic ModalShell plus the small shared pieces
// (Button / ToggleRow) that individual dialogs import

/** Generic dialog shell: height is bounded by the viewport and taller content
 *  scrolls inside, so the buttons are never clipped in a small window */
export function ModalShell({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose?: () => void;
  children: React.ReactNode;
}) {
  return (
    <div
      className="anim-fade-in fixed inset-0 z-50 flex items-center justify-center bg-abyss/75 backdrop-blur-[3px]"
      onClick={onClose}
    >
      <div
        className="anim-fade-up flex max-h-[88vh] w-[440px] max-w-[92vw] flex-col overflow-hidden rounded-xl border border-line-2 bg-panel shadow-[0_24px_80px_rgba(0,0,0,0.55)]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center justify-between border-b border-line px-5 py-3">
          <span className="gauge-label !text-sonar">{title}</span>
          {onClose && (
            <button
              onClick={onClose}
              className="cursor-pointer text-mist transition-colors hover:text-fog"
            >
              ✕
            </button>
          )}
        </div>
        <div className="min-h-0 overflow-y-auto">{children}</div>
      </div>
    </div>
  );
}

/** Primary / secondary button */
export function Button({
  children,
  onClick,
  variant = "ghost",
  disabled,
}: {
  children: React.ReactNode;
  onClick: () => void;
  variant?: "primary" | "ghost" | "danger";
  disabled?: boolean;
}) {
  const styles = {
    primary:
      "border-sonar/60 bg-sonar/15 text-sonar hover:bg-sonar/25 disabled:opacity-40",
    ghost: "border-line-2 text-fog/85 hover:border-mist hover:text-fog disabled:opacity-40",
    danger: "border-alert/40 text-alert hover:bg-alert/10 disabled:opacity-40",
  }[variant];
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`cursor-pointer rounded-md border px-3.5 py-1.5 text-sm transition-colors disabled:cursor-not-allowed ${styles}`}
    >
      {children}
    </button>
  );
}

/** Toggle row: label + optional hint + switch */
export function ToggleRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="mt-3 flex items-center gap-3">
      <div className="min-w-0 flex-1">
        <div className="text-sm text-fog">{label}</div>
        {hint && <div className="mt-0.5 text-[11px] text-mist">{hint}</div>}
      </div>
      <button
        onClick={() => onChange(!checked)}
        role="switch"
        aria-checked={checked}
        className={`relative h-5 w-9 shrink-0 cursor-pointer rounded-full border transition-colors ${
          checked ? "border-sonar/60 bg-sonar/30" : "border-line-2 bg-abyss/60"
        }`}
      >
        <span
          className={`absolute top-0.5 size-3.5 rounded-full transition-all ${
            checked ? "left-4.5 bg-sonar" : "left-0.5 bg-mist"
          }`}
        />
      </button>
    </div>
  );
}
