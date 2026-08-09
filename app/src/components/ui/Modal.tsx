import { useCallback, useEffect, useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "area[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "button:not([disabled])",
  "iframe",
  "object",
  "embed",
  '[tabindex]:not([tabindex="-1"])',
  '[contenteditable="true"]',
].join(",");

function focusableWithin(root: HTMLElement): HTMLElement[] {
  // Deliberately no `offsetParent` check: everything a dialog renders is
  // visible, and `offsetParent` is unreliable inside fixed-position overlays.
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (el) => !el.closest("[hidden]") && el.getAttribute("aria-hidden") !== "true",
  );
}

export interface ModalProps {
  /** Accessible name for the dialog. Rendered as the header unless `hideTitle`. */
  title: string;
  onClose: () => void;
  children: ReactNode;
  /** Optional sticky footer row (buttons live here). */
  footer?: ReactNode;
  /** Optional sub-header description, wired to `aria-describedby`. */
  description?: ReactNode;
  /** Tailwind width class for the dialog panel. */
  widthClassName?: string;
  /** When false, Escape / overlay click / the ✕ button do not close. */
  dismissible?: boolean;
  /** Hide the ✕ in the header (the footer usually carries a Close button). */
  hideCloseButton?: boolean;
  /** Focused on mount; falls back to the first focusable child. */
  initialFocusRef?: React.RefObject<HTMLElement | null>;
  /** Applied to the scrollable body wrapper. */
  bodyClassName?: string;
}

/**
 * The one modal primitive. Every dialog in the app renders through this so
 * `role="dialog"`, `aria-modal`, a focus trap, focus restore, Escape and
 * click-outside are implemented once instead of twelve times.
 */
export default function Modal({
  title,
  onClose,
  children,
  footer,
  description,
  widthClassName = "w-[32rem]",
  dismissible = true,
  hideCloseButton = false,
  initialFocusRef,
  bodyClassName = "",
}: ModalProps) {
  const overlayRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const titleId = useId();
  const descId = useId();

  // Remember what had focus, move focus inside, restore on unmount.
  useEffect(() => {
    restoreFocusRef.current = document.activeElement as HTMLElement | null;
    const panel = panelRef.current;
    if (panel) {
      const target =
        initialFocusRef?.current ?? focusableWithin(panel)[0] ?? panel;
      // Defer so the panel is laid out (offsetParent) before we query it.
      requestAnimationFrame(() => target.focus?.());
    }
    return () => {
      restoreFocusRef.current?.focus?.();
    };
    // Mount/unmount only — re-running would steal focus mid-interaction.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Escape closes; Tab is trapped inside the panel.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && dismissible) {
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const panel = panelRef.current;
      if (!panel) return;
      const items = focusableWithin(panel);
      if (items.length === 0) {
        e.preventDefault();
        panel.focus();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (!active || !panel.contains(active)) {
        e.preventDefault();
        first.focus();
        return;
      }
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [dismissible, onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (dismissible && e.target === overlayRef.current) onClose();
    },
    [dismissible, onClose],
  );

  return createPortal(
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4"
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descId : undefined}
        tabIndex={-1}
        className={`flex flex-col max-h-[85vh] ${widthClassName} max-w-full bg-[var(--bg-overlay)] border border-[var(--border-color)] rounded-[var(--radius-panel)]`}
        style={{ boxShadow: "var(--shadow-overlay)" }}
      >
        <div className="flex items-start justify-between gap-4 px-5 py-3 border-b border-[var(--border-color)] flex-shrink-0">
          <div className="min-w-0">
            <h2 id={titleId} className="text-sm font-semibold text-[var(--text-primary)]">
              {title}
            </h2>
            {description && (
              <p id={descId} className="mt-0.5 text-xs text-[var(--text-secondary)]">
                {description}
              </p>
            )}
          </div>
          {!hideCloseButton && dismissible && (
            <button
              type="button"
              onClick={onClose}
              aria-label="Close dialog"
              className="flex-shrink-0 w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] transition-colors"
            >
              <span aria-hidden="true">✕</span>
            </button>
          )}
        </div>

        <div className={`flex-1 min-h-0 overflow-y-auto px-5 py-4 ${bodyClassName}`}>
          {children}
        </div>

        {footer && (
          <div className="flex items-center justify-end gap-2 px-5 py-3 border-t border-[var(--border-color)] flex-shrink-0">
            {footer}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}
