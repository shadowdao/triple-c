import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { FileEntry, Project } from "../../../lib/types";
import { useFileManager } from "../../../hooks/useFileManager";
import { classifyDrop, isDropTarget } from "../../../lib/dropTarget";
import { useAppState } from "../../../store/appState";
import Button from "../../ui/Button";
import FileViewerModal from "./FileViewerModal";
import OverwriteConfirmModal from "./OverwriteConfirmModal";
import { formatBytes } from "./format";

interface Props {
  project: Project;
}

/** Key of the synthetic "go up one level" row. No listing ever contains `..`. */
const PARENT_ROW = "..";

/**
 * The project's file manager.
 *
 * Interaction model, chosen to match every desktop file manager rather than
 * the old half-and-half: **single click selects, double click opens**. That
 * moved directory navigation onto double click too — a single click used to
 * navigate, which made it impossible to select a directory in order to rename
 * it. Keyboard mirrors it exactly: Enter opens, F2 renames.
 *
 * ## Focus, and why it is a roving tabindex
 *
 * Every row used to be `tabIndex={0}`, which made a 400-entry directory about
 * twelve hundred tab stops — Tab could not get *out* of the list, let alone
 * past it — and rows are keyed by name, so navigating unmounted the focused
 * `<tr>` and dropped focus to `<body>`: Enter on a directory ejected you from
 * the grid, arrows dead, Tab restarting from the top of the document. So
 * exactly one row carries `tabIndex={0}` (the *active* row), the arrows move
 * it, and a single effect below is responsible for putting focus back on a
 * sensible row after anything that re-renders the list.
 */
export default function FilesTab({ project }: Props) {
  const {
    currentPath,
    entries,
    loading,
    error,
    busy,
    completed,
    conflict,
    resolveConflict,
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
  /** The row that owns the grid's single tab stop. */
  const [activeRow, setActiveRow] = useState<string | null>(null);

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

  // ---------------------------------------------------------------------------
  // Roving tabindex
  // ---------------------------------------------------------------------------

  /** Every row's key, in visual order. The parent row is a row like any other. */
  const rowKeys = useMemo(
    () => [
      ...(currentPath !== "/" ? [PARENT_ROW] : []),
      ...entries.map((entry) => entry.name),
    ],
    [currentPath, entries],
  );

  /**
   * The active row, resolved against what is actually on screen. Keeping the
   * *intent* in state and resolving it at render time means a rename or a
   * deletion cannot leave the grid with no tab stop at all.
   */
  const active = activeRow && rowKeys.includes(activeRow) ? activeRow : rowKeys[0];

  const rowElement = useCallback((key: string): HTMLElement | undefined => {
    // Matched on the dataset rather than a selector, because a file name is
    // user data and can contain quotes, brackets and backslashes.
    const rows = paneRef.current?.querySelectorAll<HTMLElement>("tr[data-file-row]") ?? [];
    return Array.from(rows).find((row) => row.dataset.fileRow === key);
  }, []);

  const focusRow = useCallback(
    (key: string) => {
      setActiveRow(key);
      rowElement(key)?.focus();
    },
    [rowElement],
  );

  /**
   * Where focus should land the next time the grid re-renders, if it is loose.
   * `key` is a preference, not a promise — the row may not exist any more (a
   * rename that failed, a navigation into a different directory), in which case
   * the first row takes it.
   */
  const wantFocus = useRef<{ key: string | null } | null>(null);

  /**
   * The single place that decides where focus goes after the list changes.
   *
   * Runs after a navigation (rows are keyed by name, so the focused `<tr>` is
   * gone), after a rename commits or is abandoned, and after Escape. It never
   * *steals* focus: if the user has moved on to a button or the breadcrumb it
   * drops the request instead, so a background re-list cannot yank the caret
   * out from under them.
   */
  useEffect(() => {
    if (renaming !== null) return; // the rename input owns focus
    const want = wantFocus.current;
    if (!want) return;
    wantFocus.current = null;

    const focused = document.activeElement as HTMLElement | null;
    const loose =
      !focused ||
      focused === document.body ||
      focused === document.documentElement ||
      !!focused.closest?.("tr[data-file-row]");
    if (!loose) return;

    const key = want.key && rowKeys.includes(want.key) ? want.key : rowKeys[0];
    if (key !== undefined) focusRow(key);
  }, [rowKeys, renaming, focusRow]);

  /** Arrow / Home / End movement over the rows. */
  const moveActive = useCallback(
    (from: string, to: 1 | -1 | "first" | "last") => {
      if (rowKeys.length === 0) return;
      const i = rowKeys.indexOf(from);
      const next =
        to === "first"
          ? 0
          : to === "last"
            ? rowKeys.length - 1
            : Math.min(rowKeys.length - 1, Math.max(0, (i < 0 ? 0 : i) + to));
      focusRow(rowKeys[next]);
    },
    [rowKeys, focusRow],
  );

  const startRename = useCallback((entry: FileEntry) => {
    setSelected(entry.name);
    setActiveRow(entry.name);
    setRenameDraft(entry.name);
    setRenaming(entry.name);
    // Whichever way the rename ends, focus comes back to this row unless the
    // commit renames it — `commitRename` overwrites the preference below.
    wantFocus.current = { key: entry.name };
  }, []);

  const commitRename = useCallback(
    async (entry: FileEntry) => {
      const renamedTo = renameDraft.trim();
      wantFocus.current = { key: renamedTo || entry.name };
      const done = await renameEntry(entry, renameDraft);
      if (done) setRenaming(null);
    },
    [renameEntry, renameDraft],
  );

  const commitFolder = useCallback(async () => {
    const created = folderDraft.trim();
    const done = await createFolder(folderDraft);
    if (done) {
      setCreatingFolder(false);
      setFolderDraft("");
      wantFocus.current = { key: created || null };
    }
  }, [createFolder, folderDraft]);

  /** Double click / Enter: directories navigate, files open the viewer. */
  const openEntry = useCallback(
    (entry: FileEntry) => {
      if (entry.is_directory) {
        // The new listing's first row is `..`, which is the sensible landing
        // place: it is where you go to undo the step you just took.
        wantFocus.current = { key: null };
        navigate(entry.path);
      } else {
        setViewing(entry);
      }
    },
    [navigate],
  );

  const openParent = useCallback(() => {
    // Coming back up, the directory just left is the interesting row.
    const leaving = currentPath.split("/").filter(Boolean).pop() ?? null;
    wantFocus.current = { key: leaving };
    goUp();
  }, [currentPath, goUp]);

  // Host → container drag and drop.
  //
  // This is Tauri's *native* drag-drop event, not HTML5 `ondrop`, for the same
  // reason `TerminalView` uses it: `dragDropEnabled` is on (the terminal needs
  // it), which blocks HTML5 drag inside the webview on Windows, and only the
  // native payload carries real file *paths*. The listener is window-wide, so
  // routing is `classifyDrop` — the rect hit test *plus* the z-order question
  // a rect cannot answer: is a modal or a blocking overlay painted at that
  // point? (Not "is that element mine": chrome painted over a pane — a toast,
  // a button — is not something that swallows a drop, and treating it as such
  // made permanent dead zones.)
  useEffect(() => {
    if (!running) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    (async () => {
      const un = await getCurrentWebview().onDragDropEvent(async (event) => {
        const payload = event.payload;
        if (payload.type === "leave") {
          setDragOver(false);
          return;
        }
        if (payload.type === "enter" || payload.type === "over") {
          setDragOver(isDropTarget(paneRef.current, payload.position));
          return;
        }
        if (payload.type !== "drop") return;
        setDragOver(false);
        const verdict = classifyDrop(paneRef.current, payload.position);
        // Aimed at this pane and refused anyway: say so. Nothing else would —
        // the file just never appears in the listing.
        if (verdict === "blocked") {
          console.warn(
            "[drop] refused: an overlay is covering the drop point",
            payload.position,
          );
          useAppState.getState().pushToast({
            kind: "info",
            message: "File drop ignored",
            detail:
              "A dialog or full-window overlay is covering that point. Close it and drop the file again.",
          });
          return;
        }
        if (verdict !== "accept") return;
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

  const headerClass = "px-2 py-1.5 font-medium text-[var(--text-secondary)]";

  /**
   * The live region's text. One region, always mounted, filled and emptied —
   * a `role="status"` node that is *inserted* already carrying its text is
   * frequently not announced at all, which is how "uploading 3 items…" and
   * every completion notice used to go by in silence.
   */
  const liveText = busy ? busy : (completed ?? "");

  return (
    <div ref={paneRef} className="relative flex flex-col h-full min-h-0">
      <div className="flex items-center gap-1 px-4 py-2 border-b border-[var(--border-color)] text-xs overflow-x-auto flex-shrink-0">
        <nav aria-label="Path" className="flex items-center gap-1">
          {breadcrumbs.map((crumb, i) => (
            <span key={crumb.path} className="flex items-center gap-1">
              {i > 0 && <span className="text-[var(--text-secondary)]">/</span>}
              <button
                type="button"
                onClick={() => {
                  wantFocus.current = { key: null };
                  navigate(crumb.path);
                }}
                className="text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors whitespace-nowrap font-mono"
              >
                {crumb.label}
              </button>
            </span>
          ))}
        </nav>
        <div className="flex-1" />
        <span role="status" className="mr-2 text-[var(--text-secondary)] whitespace-nowrap">
          {liveText}
        </span>
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
        {/* The one failure that stays inline: it explains why the grid below is
            empty, it is in context, and there are no rows for it to scroll
            behind. Every *transient* failure — upload, rename, mkdir,
            save-to-host — goes to `ToastHost` instead, which is above
            the file viewer's overlay and does not scroll away. */}
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
            <thead>
              <tr role="row">
                <th role="columnheader" scope="col" className={`${headerClass} px-4 text-left`}>
                  Name
                </th>
                <th role="columnheader" scope="col" className={`${headerClass} text-right`}>
                  Size
                </th>
                <th role="columnheader" scope="col" className={`${headerClass} text-left`}>
                  Modified
                </th>
                <th role="columnheader" scope="col" className={`${headerClass} text-right`}>
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {creatingFolder && (
                <tr role="row">
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
                  role="row"
                  data-file-row={PARENT_ROW}
                  tabIndex={active === PARENT_ROW ? 0 : -1}
                  aria-label="Parent directory"
                  onClick={() => setActiveRow(PARENT_ROW)}
                  onDoubleClick={openParent}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      openParent();
                    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                      e.preventDefault();
                      moveActive(PARENT_ROW, e.key === "ArrowDown" ? 1 : -1);
                    } else if (e.key === "Home" || e.key === "End") {
                      e.preventDefault();
                      moveActive(PARENT_ROW, e.key === "Home" ? "first" : "last");
                    }
                  }}
                  className="cursor-pointer hover:bg-[var(--bg-tertiary)] transition-colors"
                >
                  <td role="gridcell" className="px-4 py-1.5 text-[var(--text-primary)] font-mono">
                    <span className="sr-only">Folder, </span>
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
                    role="row"
                    data-file-row={entry.name}
                    tabIndex={active === entry.name ? 0 : -1}
                    aria-selected={isSelected}
                    onClick={() => {
                      setSelected(entry.name);
                      setActiveRow(entry.name);
                    }}
                    onDoubleClick={() => openEntry(entry)}
                    onKeyDown={(e) => {
                      if (isRenaming) return;
                      if (e.key === "Enter") {
                        e.preventDefault();
                        setSelected(entry.name);
                        setActiveRow(entry.name);
                        openEntry(entry);
                      } else if (e.key === "F2") {
                        e.preventDefault();
                        startRename(entry);
                      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                        e.preventDefault();
                        moveActive(entry.name, e.key === "ArrowDown" ? 1 : -1);
                      } else if (e.key === "Home" || e.key === "End") {
                        e.preventDefault();
                        moveActive(entry.name, e.key === "Home" ? "first" : "last");
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
                          {/* Directory-ness was carried by hue and an
                              `aria-hidden` emoji, i.e. by nothing at all for a
                              screen reader. The emoji stays hidden — it reads
                              as "file folder" in some voices and as nothing in
                              others — and the word is what is announced. */}
                          <span className="sr-only">
                            {entry.is_directory ? "Folder, " : "File, "}
                          </span>
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
                          {/* WCAG 2.5.3: the accessible name has to *contain*
                              the visible label, so the row context is appended
                              rather than substituted. "Rename notes.txt" used
                              to be the whole name, which left a voice-control
                              user saying "click Rename" at a button that had
                              no such name. */}
                          <Button
                            aria-label={`Rename — ${entry.name}`}
                            onClick={(e) => {
                              e.stopPropagation();
                              startRename(entry);
                            }}
                          >
                            Rename
                          </Button>
                          {!entry.is_directory && (
                            <Button
                              aria-label={`Save to host… — ${entry.name}`}
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
                <tr role="row">
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

      {conflict && (
        <OverwriteConfirmModal
          name={conflict.name}
          directory={conflict.directory}
          remaining={conflict.remaining}
          onChoose={resolveConflict}
        />
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
