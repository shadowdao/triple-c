import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { FileEntry, Project } from "../../../lib/types";
import { useFileManager } from "../../../hooks/useFileManager";
import Button from "../../ui/Button";
import FileViewerModal from "./FileViewerModal";
import { formatBytes } from "./format";

interface Props {
  project: Project;
}

/**
 * The project's file manager.
 *
 * Interaction model, chosen to match every desktop file manager rather than
 * the old half-and-half: **single click selects, double click opens**. That
 * moved directory navigation onto double click too — a single click used to
 * navigate, which made it impossible to select a directory in order to rename
 * it. Keyboard mirrors it exactly: Enter opens, F2 renames.
 */
export default function FilesTab({ project }: Props) {
  const {
    currentPath,
    entries,
    loading,
    error,
    busy,
    navigate,
    goUp,
    refresh,
    downloadFile,
    uploadFile,
    uploadPaths,
    renameEntry,
    createFolder,
  } = useFileManager(project.id);

  const running = project.status === "running";

  /** The row the user has selected, by name — names are unique in a directory. */
  const [selected, setSelected] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [folderDraft, setFolderDraft] = useState("");
  const [viewing, setViewing] = useState<FileEntry | null>(null);
  /** A host drag is currently over this pane. */
  const [dragOver, setDragOver] = useState(false);

  const paneRef = useRef<HTMLDivElement>(null);
  const renameInputRef = useRef<HTMLInputElement>(null);
  const folderInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (running) navigate("/workspace");
    // Re-list when the container comes up.
  }, [navigate, running]);

  // Leaving a directory invalidates every in-flight row interaction.
  useEffect(() => {
    setSelected(null);
    setRenaming(null);
  }, [currentPath]);

  useEffect(() => {
    if (renaming) {
      renameInputRef.current?.focus();
      renameInputRef.current?.select();
    }
  }, [renaming]);

  useEffect(() => {
    if (creatingFolder) folderInputRef.current?.focus();
  }, [creatingFolder]);

  const startRename = useCallback((entry: FileEntry) => {
    setSelected(entry.name);
    setRenameDraft(entry.name);
    setRenaming(entry.name);
  }, []);

  const commitRename = useCallback(
    async (entry: FileEntry) => {
      const done = await renameEntry(entry, renameDraft);
      if (done) setRenaming(null);
    },
    [renameEntry, renameDraft],
  );

  const commitFolder = useCallback(async () => {
    const done = await createFolder(folderDraft);
    if (done) {
      setCreatingFolder(false);
      setFolderDraft("");
    }
  }, [createFolder, folderDraft]);

  /**
   * Arrow keys walk the rows. `aria-selected` is only meaningful on a row
   * inside a `grid`, and a grid is expected to be arrow-navigable — so the
   * roles below and this handler come as a pair.
   */
  const moveFocus = useCallback((from: HTMLElement, delta: 1 | -1) => {
    const rows = Array.from(
      paneRef.current?.querySelectorAll<HTMLElement>('tr[tabindex="0"]') ?? [],
    );
    const i = rows.indexOf(from);
    const next = rows[i + delta];
    next?.focus();
  }, []);

  /** Double click / Enter: directories navigate, files open the viewer. */
  const openEntry = useCallback(
    (entry: FileEntry) => {
      if (entry.is_directory) navigate(entry.path);
      else setViewing(entry);
    },
    [navigate],
  );

  // Host → container drag and drop.
  //
  // This is Tauri's *native* drag-drop event, not HTML5 `ondrop`, for the same
  // reason `TerminalView` uses it: `dragDropEnabled` is on (the terminal needs
  // it), which blocks HTML5 drag inside the webview on Windows, and only the
  // native payload carries real file *paths*. The listener is window-wide, so
  // routing is a hit-test of the physical-pixel payload position against this
  // pane's rect — a hidden pane has a zero-size rect and never matches, which
  // is what keeps this and the terminal's listener from both firing.
  useEffect(() => {
    if (!running) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    const insideThisPane = (pos: { x: number; y: number }): boolean => {
      const rect = paneRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0 || rect.height === 0) return false;
      const dpr = window.devicePixelRatio || 1;
      const x = pos.x / dpr;
      const y = pos.y / dpr;
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    };

    (async () => {
      const un = await getCurrentWebview().onDragDropEvent(async (event) => {
        const payload = event.payload;
        if (payload.type === "leave") {
          setDragOver(false);
          return;
        }
        if (payload.type === "enter" || payload.type === "over") {
          setDragOver(insideThisPane(payload.position));
          return;
        }
        if (payload.type !== "drop") return;
        setDragOver(false);
        if (!insideThisPane(payload.position)) return;
        const paths = payload.paths ?? [];
        if (paths.length === 0) return;
        await uploadPaths(paths);
      });
      if (cancelled) un();
      else unlisten = un;
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [running, uploadPaths]);

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

  const rowClass = (isSelected: boolean) =>
    `cursor-pointer transition-colors ${
      isSelected
        ? "bg-[var(--bg-tertiary)]"
        : "hover:bg-[var(--bg-tertiary)]"
    }`;

  return (
    <div ref={paneRef} className="relative flex flex-col h-full min-h-0">
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
        {busy && (
          <span role="status" className="mr-2 text-[var(--text-secondary)] whitespace-nowrap">
            {busy}
          </span>
        )}
        <Button
          onClick={() => {
            setFolderDraft("");
            setCreatingFolder(true);
          }}
        >
          New folder
        </Button>
        <Button onClick={uploadFile} className="ml-1">
          Upload file
        </Button>
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
          <table role="grid" aria-label="Files" className="w-full text-xs">
            <tbody>
              {creatingFolder && (
                <tr>
                  <td role="gridcell" className="px-4 py-1.5" colSpan={4}>
                    <input
                      ref={folderInputRef}
                      value={folderDraft}
                      aria-label="New folder name"
                      placeholder="Folder name"
                      onChange={(e) => setFolderDraft(e.target.value)}
                      onBlur={commitFolder}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                        if (e.key === "Escape") {
                          setCreatingFolder(false);
                          setFolderDraft("");
                        }
                      }}
                      className="w-64 px-1 py-0 select-text bg-[var(--bg-primary)] border border-[var(--accent)] rounded-[var(--radius-control)] text-xs font-mono text-[var(--text-primary)]"
                    />
                  </td>
                </tr>
              )}
              {currentPath !== "/" && (
                <tr
                  tabIndex={0}
                  aria-label="Parent directory"
                  onDoubleClick={goUp}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      goUp();
                    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                      e.preventDefault();
                      moveFocus(e.currentTarget, e.key === "ArrowDown" ? 1 : -1);
                    }
                  }}
                  className="cursor-pointer hover:bg-[var(--bg-tertiary)] transition-colors"
                >
                  <td role="gridcell" className="px-4 py-1.5 text-[var(--text-primary)] font-mono">
                    ..
                  </td>
                  <td role="gridcell" colSpan={3} />
                </tr>
              )}
              {entries.map((entry) => {
                const isSelected = selected === entry.name;
                const isRenaming = renaming === entry.name;
                return (
                  <tr
                    key={entry.name}
                    tabIndex={0}
                    aria-selected={isSelected}
                    onClick={() => setSelected(entry.name)}
                    onDoubleClick={() => openEntry(entry)}
                    onKeyDown={(e) => {
                      if (isRenaming) return;
                      if (e.key === "Enter") {
                        e.preventDefault();
                        setSelected(entry.name);
                        openEntry(entry);
                      } else if (e.key === "F2") {
                        e.preventDefault();
                        startRename(entry);
                      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                        e.preventDefault();
                        moveFocus(e.currentTarget, e.key === "ArrowDown" ? 1 : -1);
                      }
                    }}
                    className={rowClass(isSelected)}
                  >
                    <td role="gridcell" className="px-4 py-1.5">
                      {isRenaming ? (
                        <input
                          ref={renameInputRef}
                          value={renameDraft}
                          aria-label={`New name for ${entry.name}`}
                          onChange={(e) => setRenameDraft(e.target.value)}
                          onClick={(e) => e.stopPropagation()}
                          onDoubleClick={(e) => e.stopPropagation()}
                          onBlur={() => commitRename(entry)}
                          onKeyDown={(e) => {
                            e.stopPropagation();
                            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                            if (e.key === "Escape") setRenaming(null);
                          }}
                          className="w-64 px-1 py-0 select-text bg-[var(--bg-primary)] border border-[var(--accent)] rounded-[var(--radius-control)] text-xs font-mono text-[var(--text-primary)]"
                        />
                      ) : (
                        <span
                          className={`font-mono ${
                            entry.is_directory
                              ? "text-[var(--accent)]"
                              : "text-[var(--text-primary)]"
                          }`}
                        >
                          {entry.is_directory && <span aria-hidden="true">📁 </span>}
                          <span>{entry.name}</span>
                          {entry.is_symlink && (
                            <span
                              className="ml-1 text-[var(--text-secondary)]"
                              title="Symbolic link"
                            >
                              ↗ link
                            </span>
                          )}
                        </span>
                      )}
                    </td>
                    <td role="gridcell" className="px-2 py-1.5 text-[var(--text-secondary)] text-right whitespace-nowrap tabular-nums">
                      {!entry.is_directory && formatBytes(entry.size)}
                    </td>
                    <td role="gridcell" className="px-2 py-1.5 text-[var(--text-secondary)] whitespace-nowrap">
                      {entry.modified}
                    </td>
                    <td role="gridcell" className="px-2 py-1.5 text-right whitespace-nowrap">
                      {!isRenaming && (
                        <>
                          <Button
                            aria-label={`Rename ${entry.name}`}
                            onClick={(e) => {
                              e.stopPropagation();
                              startRename(entry);
                            }}
                          >
                            Rename
                          </Button>
                          {!entry.is_directory && (
                            <Button
                              aria-label={`Save ${entry.name} to host`}
                              className="ml-1"
                              onClick={(e) => {
                                e.stopPropagation();
                                downloadFile(entry);
                              }}
                            >
                              Save to host…
                            </Button>
                          )}
                        </>
                      )}
                    </td>
                  </tr>
                );
              })}
              {entries.length === 0 && !loading && (
                <tr>
                  <td
                    role="gridcell"
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

      {/* Drop hint. Purely decorative — the native listener is what accepts the
          drop, so this must never intercept pointer events. */}
      {dragOver && (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 flex items-center justify-center border-2 border-dashed border-[var(--accent)] bg-[var(--bg-primary)]/70"
        >
          <span className="text-[13px] font-medium text-[var(--text-primary)]">
            Drop files into {currentPath}
          </span>
        </div>
      )}

      {viewing && (
        <FileViewerModal
          projectId={project.id}
          entry={viewing}
          onClose={() => setViewing(null)}
          onSaveToHost={downloadFile}
        />
      )}
    </div>
  );
}
