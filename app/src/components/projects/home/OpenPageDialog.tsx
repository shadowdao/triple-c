import { useState } from "react";
import Modal from "../../ui/Modal";
import Button from "../../ui/Button";

/**
 * Viewport presets. These are the *page's* resolution, not the window's — the
 * pane is a screencast, so a bigger window shows the same pixels drawn larger
 * while this is what actually reflows the layout.
 */
const PRESETS: { label: string; width: number; height: number }[] = [
  { label: "1280 × 720", width: 1280, height: 720 },
  { label: "1920 × 1080", width: 1920, height: 1080 },
  { label: "1440 × 900", width: 1440, height: 900 },
  { label: "390 × 844 (phone)", width: 390, height: 844 },
];

interface Props {
  /** Prefilled URL — an auth URL from the terminal, or the last one used. */
  initialUrl?: string;
  initialWidth?: number;
  initialHeight?: number;
  busy?: boolean;
  onOpen: (url: string, width: number, height: number) => void;
  onClose: () => void;
}

/**
 * Ask for a URL and a viewport, then open it in the container's browser.
 *
 * Deliberately modal and short-lived — the convention for a task with one
 * question and one button. The URL is not opened here; the caller runs the
 * command so failures land in its toast.
 */
export default function OpenPageDialog({
  initialUrl = "",
  initialWidth = 1280,
  initialHeight = 720,
  busy = false,
  onOpen,
  onClose,
}: Props) {
  const [url, setUrl] = useState(initialUrl);
  const [width, setWidth] = useState(initialWidth);
  const [height, setHeight] = useState(initialHeight);

  const trimmed = url.trim();
  // Mirrors the backend's allow-list, so the error arrives before the click
  // rather than after a round trip.
  const valid = /^https?:\/\/\S+$/i.test(trimmed);

  return (
    <Modal
      title="Open a page in the container's browser"
      onClose={onClose}
      footer={
        <>
          <Button size="md" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            size="md"
            variant="primary"
            disabled={!valid || busy}
            onClick={() => onOpen(trimmed, width, height)}
          >
            {busy ? "Opening…" : "Open page"}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <p className="text-[13px] text-[var(--text-secondary)] leading-relaxed">
          Launches a browser <em>inside</em> this container and publishes it to the
          Browser tab. Use it for a sign-in page — the callback listener is in the
          container too, so the login completes without involving your host browser —
          or for a dev server on container loopback.
        </p>

        <label className="block">
          <span className="text-xs text-[var(--text-secondary)]">URL</span>
          <input
            autoFocus
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && valid && !busy) onOpen(trimmed, width, height);
            }}
            placeholder="http://localhost:5173"
            spellCheck={false}
            className="mt-1 w-full px-2 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] font-mono text-[var(--text-primary)]"
          />
          {trimmed !== "" && !valid && (
            <span className="mt-1 block text-xs text-[var(--error)]">
              Only http:// and https:// URLs can be opened.
            </span>
          )}
        </label>

        <div>
          <span className="text-xs text-[var(--text-secondary)]">Viewport</span>
          <div className="mt-1 flex flex-wrap gap-1.5">
            {PRESETS.map((p) => {
              const active = p.width === width && p.height === height;
              return (
                <button
                  key={p.label}
                  type="button"
                  aria-pressed={active}
                  onClick={() => {
                    setWidth(p.width);
                    setHeight(p.height);
                  }}
                  className={`px-2 py-1 text-xs rounded-[var(--radius-control)] border transition-colors ${
                    active
                      ? "border-[var(--accent)] bg-[var(--accent-muted)] text-[var(--accent)]"
                      : "border-[var(--border-color)] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
                  }`}
                >
                  {p.label}
                </button>
              );
            })}
          </div>
          <div className="mt-2 flex items-center gap-2">
            <input
              type="number"
              aria-label="Viewport width"
              value={width}
              min={200}
              onChange={(e) => setWidth(Number(e.target.value))}
              className="w-24 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-xs text-[var(--text-primary)]"
            />
            <span aria-hidden="true" className="text-xs text-[var(--text-secondary)]">×</span>
            <input
              type="number"
              aria-label="Viewport height"
              value={height}
              min={200}
              onChange={(e) => setHeight(Number(e.target.value))}
              className="w-24 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-xs text-[var(--text-primary)]"
            />
            <span className="text-xs text-[var(--text-secondary)]">CSS pixels</span>
          </div>
        </div>
      </div>
    </Modal>
  );
}
