import { useEffect } from "react";
import type { Project } from "../../../lib/types";
import { useFileManager } from "../../../hooks/useFileManager";
import Button from "../../ui/Button";
import { formatBytes } from "./format";

interface Props {
  project: Project;
}

/** The old 42rem FileManager popup, now a main-area section. */
export default function FilesTab({ project }: Props) {
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
  } = useFileManager(project.id);

  const running = project.status === "running";

  useEffect(() => {
    if (running) navigate("/workspace");
    // Re-list when the container comes up.
  }, [navigate, running]);

  const breadcrumbs =
    currentPath === "/"
      ? [{ label: "/", path: "/" }]
      : currentPath
          .split("/")
          .reduce<{ label: string; path: string }[]>((acc, part, i) => {
            if (i === 0) {
              acc.push({ label: "/", path: "/" });
            } else if (part) {
              const parentPath = acc[acc.length - 1].path;
              const fullPath = parentPath === "/" ? `/${part}` : `${parentPath}/${part}`;
              acc.push({ label: part, path: fullPath });
            }
            return acc;
          }, []);

  if (!running) {
    return (
      <div className="p-4">
        <p className="text-[13px] text-[var(--text-secondary)]">
          Start the container to browse its files.
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-center gap-1 px-4 py-2 border-b border-[var(--border-color)] text-xs overflow-x-auto flex-shrink-0">
        <nav aria-label="Path" className="flex items-center gap-1">
          {breadcrumbs.map((crumb, i) => (
            <span key={crumb.path} className="flex items-center gap-1">
              {i > 0 && <span className="text-[var(--text-secondary)]">/</span>}
              <button
                type="button"
                onClick={() => navigate(crumb.path)}
                className="text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors whitespace-nowrap font-mono"
              >
                {crumb.label}
              </button>
            </span>
          ))}
        </nav>
        <div className="flex-1" />
        <Button onClick={uploadFile}>Upload file</Button>
        <Button onClick={refresh} disabled={loading} className="ml-1">
          Refresh
        </Button>
      </div>

      <div className="flex-1 overflow-y-auto min-h-0">
        {error && (
          <div role="alert" className="px-4 py-2 text-xs text-[var(--error)]">
            {error}
          </div>
        )}

        {loading && entries.length === 0 ? (
          <div className="px-4 py-8 text-center text-xs text-[var(--text-secondary)]">
            Loading…
          </div>
        ) : (
          <table className="w-full text-xs">
            <tbody>
              {currentPath !== "/" && (
                <tr
                  onClick={goUp}
                  className="cursor-pointer hover:bg-[var(--bg-tertiary)] transition-colors"
                >
                  <td className="px-4 py-1.5 text-[var(--text-primary)] font-mono">..</td>
                  <td colSpan={3} />
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
                    <span
                      className={`font-mono ${
                        entry.is_directory
                          ? "text-[var(--accent)]"
                          : "text-[var(--text-primary)]"
                      }`}
                    >
                      {entry.is_directory ? "📁 " : ""}
                      {entry.name}
                    </span>
                  </td>
                  <td className="px-2 py-1.5 text-[var(--text-secondary)] text-right whitespace-nowrap tabular-nums">
                    {!entry.is_directory && formatBytes(entry.size)}
                  </td>
                  <td className="px-2 py-1.5 text-[var(--text-secondary)] whitespace-nowrap">
                    {entry.modified}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {!entry.is_directory && (
                      <Button
                        aria-label={`Download ${entry.name}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          downloadFile(entry);
                        }}
                      >
                        Download
                      </Button>
                    )}
                  </td>
                </tr>
              ))}
              {entries.length === 0 && !loading && (
                <tr>
                  <td
                    colSpan={4}
                    className="px-4 py-8 text-center text-[var(--text-secondary)]"
                  >
                    Empty directory
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
