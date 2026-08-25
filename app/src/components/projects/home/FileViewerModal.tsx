import { useEffect, useRef, useState } from "react";
import type { FileEntry } from "../../../lib/types";
import { readContainerFile } from "../../../lib/tauri-commands";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";
import { formatBytes } from "./format";
import {
  decodeBase64,
  imageMimeFor,
  looksBinary,
  previewKind,
  previewLimit,
} from "./filePreview";

interface Props {
  projectId: string;
  entry: FileEntry;
  onClose: () => void;
}

type Preview =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  /** Too big to render whole — said so rather than shown as a half-file. */
  | { kind: "too-large" }
  | { kind: "text"; text: string; truncated: boolean; shownBytes: number; trueSize: number }
  | { kind: "image"; url: string }
  | { kind: "unsupported" };

/**
 * Read-only preview of one container file.
 *
 * Images are rendered from a `blob:` URL rather than a `data:` one — the object
 * URL is revocable (so the bytes are released the moment the modal closes) and
 * keeps a multi-megabyte base64 string out of the DOM. `blob:` is in the app's
 * `img-src` for exactly this; the asset protocol deliberately is not enabled.
 */
export default function FileViewerModal({ projectId, entry, onClose }: Props) {
  const [preview, setPreview] = useState<Preview>({ kind: "loading" });

  /**
   * The object URL currently on screen.
   *
   * This used to be an effect-local variable revoked from the effect's own
   * cleanup, which runs *before* the replacement effect body — so switching
   * entries (or any re-run of the effect for the same entry) released the URL
   * the `<img>` was still pointing at, and a blank image was the result until
   * the new read landed. If the new read failed, it stayed blank. So the
   * hand-over is explicit instead: a URL is revoked only once its replacement
   * exists, and unmount is what releases the last one.
   */
  const objectUrlRef = useRef<string | null>(null);

  /** Release the previous URL now that something else is on screen. */
  const replaceObjectUrl = (next: string | null) => {
    const previous = objectUrlRef.current;
    objectUrlRef.current = next;
    if (previous && previous !== next) URL.revokeObjectURL(previous);
  };

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const wantImage = previewKind(entry.name) === "image";
        const result = await readContainerFile(projectId, entry.path, previewLimit(entry.name));
        if (cancelled) return;

        const bytes = decodeBase64(result.contents_base64);

        if (wantImage) {
          // A truncated image is not a smaller image, it is a broken one.
          if (result.truncated) {
            setPreview({ kind: "too-large" });
            replaceObjectUrl(null);
            return;
          }
          const blob = new Blob([bytes], { type: imageMimeFor(entry.name) ?? "image/png" });
          const url = URL.createObjectURL(blob);
          // The replacement is in hand, so the previous one can go.
          setPreview({ kind: "image", url });
          replaceObjectUrl(url);
          return;
        }

        if (looksBinary(bytes)) {
          setPreview({ kind: "unsupported" });
          replaceObjectUrl(null);
          return;
        }

        setPreview({
          kind: "text",
          text: new TextDecoder().decode(bytes),
          truncated: result.truncated,
          shownBytes: bytes.length,
          trueSize: result.size,
        });
        replaceObjectUrl(null);
      } catch (e) {
        if (!cancelled) setPreview({ kind: "error", message: String(e) });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [projectId, entry.name, entry.path]);

  // The bytes are released when the dialog goes, which is the whole reason the
  // preview is a `blob:` URL rather than a `data:` one.
  useEffect(
    () => () => {
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
    },
    [],
  );

  const footer = (
    <Button size="md" variant="primary" onClick={onClose}>
      Close
    </Button>
  );

  return (
    <Modal
      title={entry.name}
      description={`${entry.path} · ${formatBytes(entry.size)}`}
      onClose={onClose}
      footer={footer}
      widthClassName="w-[52rem]"
    >
      {preview.kind === "loading" && (
        <p className="text-[13px] text-[var(--text-secondary)]">Loading…</p>
      )}

      {preview.kind === "error" && (
        <p role="alert" className="text-[13px] text-[var(--error)]">
          {preview.message}
        </p>
      )}

      {preview.kind === "too-large" && (
        <p className="text-[13px] text-[var(--text-secondary)]">
          This file is {formatBytes(entry.size)} — too large to preview in the app. Use
          “Save to host…” on its row to open it in a program that can, or read it from a
          terminal in the container.
        </p>
      )}

      {preview.kind === "unsupported" && (
        <p className="text-[13px] text-[var(--text-secondary)]">
          There is no preview for this file type. Use “Save to host…” on its row to open it
          in a program that can, or read it from a terminal in the container.
        </p>
      )}

      {preview.kind === "text" && (
        <>
          {preview.truncated && (
            <p className="mb-2 text-xs text-[var(--warning)]">
              Showing the first {formatBytes(preview.shownBytes)} of {formatBytes(preview.trueSize)}.
            </p>
          )}
          {/* Focusable, and its own scroll container, because a megabyte of
              text in an unfocusable `<pre>` is reachable by mouse wheel and by
              nothing else — no PageDown, no arrows, no keyboard at all. A
              scrollable region needs an accessible name to be worth landing
              on, hence the role and label. No `focus:outline-none`: the global
              `:focus-visible` ring is what says where the caret went. */}
          <pre
            tabIndex={0}
            role="region"
            aria-label={`${entry.name} contents`}
            className="max-h-[60vh] overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-[var(--text-primary)]"
          >
            {preview.text}
          </pre>
        </>
      )}

      {preview.kind === "image" && (
        <img src={preview.url} alt={entry.name} className="max-w-full mx-auto" />
      )}
    </Modal>
  );
}
