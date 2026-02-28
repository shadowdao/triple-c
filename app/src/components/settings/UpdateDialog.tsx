import { useEffect, useRef, useCallback } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { UpdateInfo } from "../../lib/types";

interface Props {
  updateInfo: UpdateInfo;
  currentVersion: string;
  onDismiss: () => void;
  onClose: () => void;
}

export default function UpdateDialog({
  updateInfo,
  currentVersion,
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

  const handleDownload = async (url: string) => {
    try {
      await openUrl(url);
    } catch (e) {
      console.error("Failed to open URL:", e);
    }
  };

  const formatSize = (bytes: number) => {
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[28rem] max-h-[80vh] overflow-y-auto shadow-xl">
        <h2 className="text-lg font-semibold mb-3">Update Available</h2>

        <div className="flex items-center gap-2 mb-4 text-sm">
          <span className="text-[var(--text-secondary)]">{currentVersion}</span>
          <span className="text-[var(--text-secondary)]">&rarr;</span>
          <span className="text-[var(--accent)] font-semibold">
            {updateInfo.version}
          </span>
        </div>

        {updateInfo.body && (
          <div className="mb-4">
            <h3 className="text-xs font-semibold uppercase text-[var(--text-secondary)] mb-1">
              Release Notes
            </h3>
            <div className="text-xs text-[var(--text-primary)] whitespace-pre-wrap bg-[var(--bg-primary)] rounded p-3 max-h-48 overflow-y-auto border border-[var(--border-color)]">
              {updateInfo.body}
            </div>
          </div>
        )}

        {updateInfo.assets.length > 0 && (
          <div className="mb-4 space-y-1">
            <h3 className="text-xs font-semibold uppercase text-[var(--text-secondary)] mb-1">
              Downloads
            </h3>
            {updateInfo.assets.map((asset) => (
              <button
                key={asset.name}
                onClick={() => handleDownload(asset.browser_download_url)}
                className="w-full flex items-center justify-between px-3 py-2 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:border-[var(--accent)] transition-colors"
              >
                <span className="truncate">{asset.name}</span>
                <span className="text-[var(--text-secondary)] ml-2 flex-shrink-0">
                  {formatSize(asset.size)}
                </span>
              </button>
            ))}
          </div>
        )}

        <div className="flex items-center justify-between">
          <button
            onClick={() => handleDownload(updateInfo.release_url)}
            className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
          >
            View on Gitea
          </button>
          <div className="flex gap-2">
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
    </div>
  );
}
