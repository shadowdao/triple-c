import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import Button from "../ui/Button";
import { inspectCaCertPath } from "../../lib/tauri-commands";
import type { CaCertInfo } from "../../lib/types";

interface Props {
  /** Wired to the calling `Field`'s label, where there is one. */
  id?: string;
  value: string;
  onChange: (value: string) => void;
  /** Persist the value — called on blur and immediately after a Browse. */
  onCommit: (value: string) => void;
  disabled?: boolean;
  placeholder?: string;
  /** Shown in place of the status line while the field is empty. */
  emptyHint?: string;
  /** Tailwind classes for the text input, so each caller keeps its local
   *  convention (the host settings panel and the project Config tab do not
   *  style their inputs the same way). */
  inputClassName: string;
}

/**
 * Path field for a corporate CA certificate — a single file *or* a directory
 * of them — shared by the global setting and the per-project override.
 *
 * Two Browse buttons rather than one: the platform file dialog cannot offer
 * "a file or a folder" in a single call, and which one the user wants is not
 * guessable (a lone `corp-root.pem` is as common as a folder of chained certs).
 *
 * The status line is what makes the feature debuggable. It reports the
 * certificate count and, crucially, the `.crt` names each file is installed
 * as: `update-ca-certificates` matches `*.crt` case-sensitively and ignores a
 * `.pem` in complete silence, so seeing `corp-root.pem → corp-root.crt` is the
 * difference between trusting the setting and guessing at it.
 */
export default function CaCertPathInput({
  id,
  value,
  onChange,
  onCommit,
  disabled = false,
  placeholder,
  emptyHint,
  inputClassName,
}: Props) {
  const [info, setInfo] = useState<CaCertInfo | null>(null);
  // Guards against a slow inspect for an earlier value landing after a newer
  // one and describing the wrong path.
  const requestId = useRef(0);

  useEffect(() => {
    const trimmed = value.trim();
    if (!trimmed) {
      setInfo(null);
      return;
    }
    const id = ++requestId.current;
    const timer = setTimeout(() => {
      inspectCaCertPath(trimmed)
        .then((result) => {
          if (requestId.current === id) setInfo(result);
        })
        .catch(() => {
          if (requestId.current === id) setInfo(null);
        });
    }, 250);
    return () => clearTimeout(timer);
  }, [value]);

  const browse = async (directory: boolean) => {
    const selected = await open({ directory, multiple: false });
    if (typeof selected === "string") {
      onChange(selected);
      onCommit(selected);
    }
  };

  return (
    <div className="space-y-1.5">
      <div className="flex gap-1.5">
        <input
          id={id}
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onBlur={() => onCommit(value)}
          placeholder={placeholder}
          disabled={disabled}
          className={inputClassName}
        />
        <Button size="md" disabled={disabled} onClick={() => browse(false)}>
          File…
        </Button>
        <Button size="md" disabled={disabled} onClick={() => browse(true)}>
          Folder…
        </Button>
      </div>
      <CaCertStatus value={value} info={info} emptyHint={emptyHint} />
    </div>
  );
}

function CaCertStatus({
  value,
  info,
  emptyHint,
}: {
  value: string;
  info: CaCertInfo | null;
  emptyHint?: string;
}) {
  if (!value.trim()) {
    return emptyHint ? (
      <p className="text-xs text-[var(--text-secondary)]">{emptyHint}</p>
    ) : null;
  }
  if (!info) return null;

  if (info.error) {
    // Glyph + word, never colour alone.
    return (
      <p className="text-xs text-[var(--error)]" role="status">
        <span aria-hidden="true">✕ </span>
        Problem: {info.error}
      </p>
    );
  }
  if (info.cert_count === 0) return null;

  return (
    <p className="text-xs text-[var(--success)]" role="status">
      <span aria-hidden="true">✓ </span>
      Found {info.cert_count} certificate{info.cert_count === 1 ? "" : "s"}
      {info.installed_names.length > 0 && (
        <span className="text-[var(--text-secondary)]">
          {" "}
          — installed as {info.installed_names.slice(0, 4).join(", ")}
          {info.installed_names.length > 4
            ? ` and ${info.installed_names.length - 4} more`
            : ""}
        </span>
      )}
    </p>
  );
}
