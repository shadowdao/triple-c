import { useState, useRef, useEffect, type ReactNode } from "react";

interface TooltipProps {
  text: string;
  children?: ReactNode;
}

/**
 * A small circled question-mark icon that shows a tooltip on hover.
 * Renders inline and automatically repositions to stay within the viewport.
 */
export default function Tooltip({ text, children }: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const [position, setPosition] = useState<"top" | "bottom">("top");
  const [align, setAlign] = useState<"center" | "left" | "right">("center");
  const triggerRef = useRef<HTMLSpanElement>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!visible || !triggerRef.current || !tooltipRef.current) return;

    const triggerRect = triggerRef.current.getBoundingClientRect();
    const tooltipRect = tooltipRef.current.getBoundingClientRect();

    // Decide vertical position: prefer top, fall back to bottom
    if (triggerRect.top - tooltipRect.height - 6 < 4) {
      setPosition("bottom");
    } else {
      setPosition("top");
    }

    // Decide horizontal alignment
    const centerLeft = triggerRect.left + triggerRect.width / 2 - tooltipRect.width / 2;
    const centerRight = centerLeft + tooltipRect.width;
    if (centerLeft < 4) {
      setAlign("left");
    } else if (centerRight > window.innerWidth - 4) {
      setAlign("right");
    } else {
      setAlign("center");
    }
  }, [visible]);

  const positionClasses = position === "top" ? "bottom-full mb-1.5" : "top-full mt-1.5";

  const alignClasses =
    align === "left"
      ? "left-0"
      : align === "right"
        ? "right-0"
        : "left-1/2 -translate-x-1/2";

  return (
    <span
      ref={triggerRef}
      className="relative inline-flex items-center ml-1"
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
      {visible && (
        <div
          ref={tooltipRef}
          className={`absolute z-50 ${positionClasses} ${alignClasses} px-2.5 py-1.5 text-[11px] leading-snug text-[var(--text-primary)] bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded shadow-lg whitespace-normal max-w-[220px] w-max pointer-events-none`}
        >
          {text}
        </div>
      )}
    </span>
  );
}
