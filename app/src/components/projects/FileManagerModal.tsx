import { useEffect, useRef, useCallback } from "react";
import { useFileManager } from "../../hooks/useFileManager";

interface Props {
  projectId: string;
  projectName: string;
  onClose: () => void;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export default function FileManagerModal({ projectId, projectName, onClose }: Props) {
  const {
    currentPath,
    entries,
    loading,
    error,
    navigate,
    goUp,
    refresh,
    downloadFile,
    uploadFile,
  } = useFileManager(projectId);
  const overlayRef = useRef<HTMLDivElement>(null);

  // Load initial directory
  useEffect(() => {
    navigate("/workspace");
  }, [navigate]);

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

  // Build breadcrumbs from current path
  const breadcrumbs = currentPath === "/"
    ? [{ label: "/", path: "/" }]
    : currentPath.split("/").reduce<{ label: string; path: string }[]>((acc, part, i) => {
        if (i === 0) {
          acc.push({ label: "/", path: "/" });
        } else if (part) {
          const parentPath = acc[acc.length - 1].path;
          const fullPath = parentPath === "/" ? `/${part}` : `${parentPath}/${part}`;
          acc.push({ label: part, path: fullPath });
        }
        return acc;
      }, []);

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg shadow-xl w-[36rem] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[var(--border-color)]">
          <h2 className="text-sm font-semibold">Files — {projectName}</h2>
          <button
            onClick={onClose}
            className="text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            ×
          </button>
        </div>

        {/* Path bar */}
        <div className="flex items-center gap-1 px-4 py-2 border-b border-[var(--border-color)] text-xs overflow-x-auto flex-shrink-0">
          {breadcrumbs.map((crumb, i) => (
            <span key={crumb.path} className="flex items-center gap-1">
              {i > 0 && <span className="text-[var(--text-secondary)]">/</span>}
              <button
                onClick={() => navigate(crumb.path)}
                className="text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors whitespace-nowrap"
              >
                {crumb.label}
              </button>
            </span>
          ))}
          <div className="flex-1" />
          <button
            onClick={refresh}
            disabled={loading}
            className="text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors disabled:opacity-50 px-1"
            title="Refresh"
          >
            ↻
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto min-h-0">
          {error && (
            <div className="px-4 py-2 text-xs text-[var(--error)]">{error}</div>
          )}

          {loading && entries.length === 0 ? (
            <div className="px-4 py-8 text-center text-xs text-[var(--text-secondary)]">
              Loading...
            </div>
          ) : (
            <table className="w-full text-xs">
              <tbody>
                {/* Go up entry */}
                {currentPath !== "/" && (
                  <tr
                    onClick={() => goUp()}
                    className="cursor-pointer hover:bg-[var(--bg-tertiary)] transition-colors"
                  >
                    <td className="px-4 py-1.5 text-[var(--text-primary)]">..</td>
                    <td></td>
                    <td></td>
                    <td></td>
                  </tr>
                )}
                {entries.map((entry) => (
                  <tr
                    key={entry.name}
                    onClick={() => entry.is_directory && navigate(entry.path)}
                    className={`${
                      entry.is_directory ? "cursor-pointer" : ""
                    } hover:bg-[var(--bg-tertiary)] transition-colors`}
                  >
                    <td className="px-4 py-1.5">
                      <span className={entry.is_directory ? "text-[var(--accent)]" : "text-[var(--text-primary)]"}>
                        {entry.is_directory ? "📁 " : ""}{entry.name}
                      </span>
                    </td>
                    <td className="px-2 py-1.5 text-[var(--text-secondary)] text-right whitespace-nowrap">
                      {!entry.is_directory && formatSize(entry.size)}
                    </td>
                    <td className="px-2 py-1.5 text-[var(--text-secondary)] whitespace-nowrap">
                      {entry.modified}
                    </td>
                    <td className="px-2 py-1.5 text-right">
                      {!entry.is_directory && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            downloadFile(entry);
                          }}
                          className="text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors px-1"
                          title="Download"
                        >
                          ↓
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
                {entries.length === 0 && !loading && (
                  <tr>
                    <td colSpan={4} className="px-4 py-8 text-center text-[var(--text-secondary)]">
                      Empty directory
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-[var(--border-color)]">
          <button
            onClick={uploadFile}
            className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
          >
            Upload file
          </button>
          <button
            onClick={onClose}
            className="px-4 py-1.5 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
