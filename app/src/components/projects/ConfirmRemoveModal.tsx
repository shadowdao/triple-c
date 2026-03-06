import { useEffect, useRef, useCallback } from "react";

interface Props {
  projectName: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export default function ConfirmRemoveModal({ projectName, onConfirm, onCancel }: Props) {
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onCancel]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onCancel();
    },
    [onCancel],
  );

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[24rem] shadow-xl">
        <h2 className="text-lg font-semibold mb-3">Remove Project</h2>
        <p className="text-sm text-[var(--text-secondary)] mb-5">
          Are you sure you want to remove <strong className="text-[var(--text-primary)]">{projectName}</strong>? This will delete the container, config volume, and stored credentials.
        </p>
        <div className="flex justify-end gap-2">
          <button
            onClick={onCancel}
            className="px-4 py-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={onConfirm}
            className="px-4 py-2 text-sm text-white bg-[var(--error)] hover:opacity-80 rounded transition-colors"
          >
            Remove
          </button>
        </div>
      </div>
    </div>
  );
}
