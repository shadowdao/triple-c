import { openUrl } from "@tauri-apps/plugin-opener";
import type { UpdateInfo } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import { formatBytes } from "../../lib/formatBytes";

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
  const handleDownload = async (url: string) => {
    try {
      await openUrl(url);
    } catch (e) {
      console.error("Failed to open URL:", e);
    }
  };

  return (
    <Modal
      title="Update Available"
      onClose={onClose}
      widthClassName="w-[30rem]"
      footer={
        <>
          <Button
            variant="ghost"
            className="mr-auto text-[var(--accent)] hover:text-[var(--accent-hover)]"
            onClick={() => handleDownload(updateInfo.release_url)}
          >
            View on Gitea
          </Button>
          <Button variant="ghost" onClick={onDismiss}>
            Dismiss
          </Button>
          <Button onClick={onClose}>Close</Button>
        </>
      }
    >
      <div className="flex items-center gap-2 mb-4 text-[13px]">
        <span className="text-[var(--text-secondary)] font-mono">{currentVersion}</span>
        <span className="text-[var(--text-secondary)]">&rarr;</span>
        <span className="text-[var(--accent)] font-semibold font-mono">
          {updateInfo.version}
        </span>
      </div>

      {updateInfo.body && (
        <div className="mb-4">
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)] mb-1">
            Release notes
          </h3>
          <div className="text-xs text-[var(--text-primary)] whitespace-pre-wrap bg-[var(--bg-primary)] rounded-[var(--radius-control)] p-3 max-h-48 overflow-y-auto border border-[var(--border-color)]">
            {updateInfo.body}
          </div>
        </div>
      )}

      {updateInfo.assets.length > 0 && (
        <div className="space-y-1">
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)] mb-1">
            Downloads
          </h3>
          {updateInfo.assets.map((asset) => (
            <button
              key={asset.name}
              type="button"
              onClick={() => handleDownload(asset.browser_download_url)}
              className="w-full flex items-center justify-between px-3 py-2 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] hover:border-[var(--accent)] transition-colors"
            >
              <span className="truncate font-mono">{asset.name}</span>
              <span className="text-[var(--text-secondary)] ml-2 flex-shrink-0">
                {/* `binary` because a release asset's size is the ÷1024 figure
                    every OS file browser shows for the same download. This
                    used to be a local copy that rendered KB whole and stopped
                    the ladder at MB; see `formatBytes.ts`. */}
                {formatBytes(asset.size, { binary: true })}
              </span>
            </button>
          ))}
        </div>
      )}
    </Modal>
  );
}
