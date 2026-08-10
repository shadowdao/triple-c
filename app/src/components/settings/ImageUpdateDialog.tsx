import type { ImageUpdateInfo } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";

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
  const shortDigest = (digest: string) => {
    // Show first 16 chars of the hash part (after "sha256:")
    const hash = digest.startsWith("sha256:") ? digest.slice(7) : digest;
    return hash.slice(0, 16);
  };

  return (
    <Modal
      title="Container Image Update"
      onClose={onClose}
      widthClassName="w-[30rem]"
      footer={
        <>
          <Button variant="ghost" onClick={onDismiss}>
            Dismiss
          </Button>
          <Button onClick={onClose}>Close</Button>
        </>
      }
    >
      <p className="text-[13px] text-[var(--text-secondary)] mb-4">
        A newer version of the container image is available in the registry. Re-pull the
        image in Docker settings to get the latest tools and fixes.
      </p>

      <div className="space-y-2 mb-4 text-xs bg-[var(--bg-primary)] rounded-[var(--radius-control)] p-3 border border-[var(--border-color)]">
        {imageUpdateInfo.local_digest && (
          <div className="flex justify-between">
            <span className="text-[var(--text-secondary)]">Local digest</span>
            <span className="font-mono text-[var(--text-primary)]">
              {shortDigest(imageUpdateInfo.local_digest)}…
            </span>
          </div>
        )}
        <div className="flex justify-between">
          <span className="text-[var(--text-secondary)]">Remote digest</span>
          <span className="font-mono text-[var(--accent)]">
            {shortDigest(imageUpdateInfo.remote_digest)}…
          </span>
        </div>
      </div>

      <p className="text-xs text-[var(--text-secondary)]">
        Go to Settings &gt; Container and click &quot;Re-pull Image&quot; to update.
        Running containers will not be affected until restarted.
      </p>
    </Modal>
  );
}
