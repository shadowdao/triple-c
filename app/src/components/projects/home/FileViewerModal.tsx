import { useEffect, useState } from "react";
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
  /** "Save to host…" — the way out for anything the viewer can't render. */
  onSaveToHost: (entry: FileEntry) => void;
}

type Preview =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  /** Too big to render whole — offered as a download rather than a half-file. */
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
export default function FileViewerModal({ projectId, entry, onClose, onSaveToHost }: Props) {
  const [preview, setPreview] = useState<Preview>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    // Tracked separately from `preview` so cleanup can revoke it without
    // depending on which state the component ended up in.
    let objectUrl: string | null = null;

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
            return;
          }
          const blob = new Blob([bytes], { type: imageMimeFor(entry.name) ?? "image/png" });
          objectUrl = URL.createObjectURL(blob);
          setPreview({ kind: "image", url: objectUrl });
          return;
        }

        if (looksBinary(bytes)) {
          setPreview({ kind: "unsupported" });
          return;
        }

        setPreview({
          kind: "text",
          text: new TextDecoder().decode(bytes),
          truncated: result.truncated,
          shownBytes: bytes.length,
          trueSize: result.size,
        });
      } catch (e) {
        if (!cancelled) setPreview({ kind: "error", message: String(e) });
      }
    })();

    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [projectId, entry.name, entry.path]);

  const footer = (
    <>
      <Button
        size="md"
        onClick={() => {
          onSaveToHost(entry);
        }}
      >
        Save to host…
      </Button>
      <Button size="md" variant="primary" onClick={onClose}>
        Close
      </Button>
    </>
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
          This file is {formatBytes(entry.size)} — too large to preview in the app. Save it
          to the host to open it there.
        </p>
      )}

      {preview.kind === "unsupported" && (
        <p className="text-[13px] text-[var(--text-secondary)]">
          There is no preview for this file type. Save it to the host to open it there.
        </p>
      )}

      {preview.kind === "text" && (
        <>
          {preview.truncated && (
            <p className="mb-2 text-xs text-[var(--warning)]">
              Showing the first {formatBytes(preview.shownBytes)} of {formatBytes(preview.trueSize)}.
            </p>
          )}
          <pre className="whitespace-pre-wrap break-words font-mono text-xs text-[var(--text-primary)]">
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
