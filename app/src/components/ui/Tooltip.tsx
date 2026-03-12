import { useState, useRef, useLayoutEffect, type ReactNode } from "react";
import { createPortal } from "react-dom";

interface TooltipProps {
  text: string;
  children?: ReactNode;
}

/**
 * A small circled question-mark icon that shows a tooltip on hover.
 * Uses a portal to render at `document.body` so the tooltip is never
 * clipped by ancestor `overflow: hidden` containers.
 */
export default function Tooltip({ text, children }: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const [coords, setCoords] = useState({ top: 0, left: 0 });
  const [, setPlacement] = useState<"top" | "bottom">("top");
  const triggerRef = useRef<HTMLSpanElement>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!visible || !triggerRef.current || !tooltipRef.current) return;

    const trigger = triggerRef.current.getBoundingClientRect();
    const tooltip = tooltipRef.current.getBoundingClientRect();
    const gap = 6;

    // Vertical: prefer above, fall back to below
    const above = trigger.top - tooltip.height - gap >= 4;
    const pos = above ? "top" : "bottom";
    setPlacement(pos);

    const top =
      pos === "top"
        ? trigger.top - tooltip.height - gap
        : trigger.bottom + gap;

    // Horizontal: center on trigger, clamp to viewport
    let left = trigger.left + trigger.width / 2 - tooltip.width / 2;
    left = Math.max(4, Math.min(left, window.innerWidth - tooltip.width - 4));

    setCoords({ top, left });
  }, [visible]);

  return (
    <span
      ref={triggerRef}
      className="inline-flex items-center ml-1"
      onMouseEnter={() => setVisible(true)}
      onMouseLeave={() => setVisible(false)}
    >
      {children ?? (
        <span
          className="inline-flex items-center justify-center w-3.5 h-3.5 rounded-full border border-[var(--text-secondary)] text-[var(--text-secondary)] text-[9px] leading-none cursor-help select-none hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors"
          aria-label="Help"
        >
          ?
        </span>
      )}
      {visible &&
        createPortal(
          <div
            ref={tooltipRef}
            style={{
              position: "fixed",
              top: coords.top,
              left: coords.left,
              zIndex: 9999,
            }}
            className={`px-2.5 py-1.5 text-[11px] leading-snug text-[var(--text-primary)] bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded shadow-lg whitespace-normal max-w-[280px] w-max pointer-events-none`}
          >
            {text}
          </div>,
          document.body
        )}
    </span>
  );
}
