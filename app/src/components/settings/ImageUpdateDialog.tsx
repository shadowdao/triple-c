import { useEffect, useRef, useCallback } from "react";
import type { ImageUpdateInfo } from "../../lib/types";

interface Props {
  imageUpdateInfo: ImageUpdateInfo;
  onDismiss: () => void;
  onClose: () => void;
}

export default function ImageUpdateDialog({
  imageUpdateInfo,
  onDismiss,
  onClose,
}: Props) {
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onClose();
    },
    [onClose],
  );

  const shortDigest = (digest: string) => {
    // Show first 16 chars of the hash part (after "sha256:")
    const hash = digest.startsWith("sha256:") ? digest.slice(7) : digest;
    return hash.slice(0, 16);
  };

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[28rem] max-h-[80vh] overflow-y-auto shadow-xl">
        <h2 className="text-lg font-semibold mb-3">Container Image Update</h2>

        <p className="text-sm text-[var(--text-secondary)] mb-4">
          A newer version of the container image is available in the registry.
          Re-pull the image in Docker settings to get the latest tools and fixes.
        </p>

        <div className="space-y-2 mb-4 text-xs bg-[var(--bg-primary)] rounded p-3 border border-[var(--border-color)]">
          {imageUpdateInfo.local_digest && (
            <div className="flex justify-between">
              <span className="text-[var(--text-secondary)]">Local digest</span>
              <span className="font-mono text-[var(--text-primary)]">
                {shortDigest(imageUpdateInfo.local_digest)}...
              </span>
            </div>
          )}
          <div className="flex justify-between">
            <span className="text-[var(--text-secondary)]">Remote digest</span>
            <span className="font-mono text-[var(--accent)]">
              {shortDigest(imageUpdateInfo.remote_digest)}...
            </span>
          </div>
        </div>

        <p className="text-xs text-[var(--text-secondary)] mb-4">
          Go to Settings &gt; Docker and click &quot;Re-pull Image&quot; to update.
          Running containers will not be affected until restarted.
        </p>

        <div className="flex items-center justify-end gap-2">
          <button
            onClick={onDismiss}
            className="px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Dismiss
          </button>
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
