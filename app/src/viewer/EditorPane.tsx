import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Extension } from "@codemirror/state";
import Button from "../components/ui/Button";
import StatusIndicator, { type StatusTone } from "../components/ui/StatusIndicator";
import { decodeBase64, encodeBase64, imageMimeFor, previewLimit } from "../components/projects/home/filePreview";
import { viewerPollFile, viewerReadFile, viewerWriteFile } from "../lib/tauri-commands";
import type { ViewerFile, ViewerLocation, ViewerState } from "../lib/types";
import { CodeEditor, type CodeEditorHandle } from "./CodeEditor";
import { CONFLICT_PREFIX, GONE_PREFIX, READ_ONLY_MESSAGE } from "./ipcMessages";
import { classifyViewerFile, type Editability } from "./editability";
import { languageFor, wrapsLines } from "./languages";
import { decodeViewerText, encodeViewerText, type TextFormat } from "./textFormat";
import { useViewerPolling } from "./useViewerPolling";
import { canSave, initialViewerState, pollEffect, reduceViewer } from "./viewerState";

const POLL_MS = 2000;
export const GOTO_EVENT = "file-viewer-goto";

const READ_ONLY_SAVE =
  "This file is read-only for the container user, so it was not saved. Your text is kept: change the file's permissions in the container and save again, or copy your text out.";
const CONFLICT_UNCHECKED =
  "The file changed on disk, but its new version could not be checked, so it cannot be overwritten safely. Copy your text out if you need it, then reload.";

/** A banner-worthy save failure; `reload` adds a "Reload (discard mine)" button. */
interface SaveError { text: string; reload?: boolean }

type View =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "text"; doc: string; editability: Editability }
  | { kind: "image"; url: string; editability: Editability }
  | { kind: "binary"; editability: Editability };

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** `write.rs`'s refusal to replace a file the container user may not write. */
const isReadOnlyRefusal = (msg: string) => msg.includes(READ_ONLY_MESSAGE);

export default function EditorPane({ state }: { state: ViewerState }) {
  const path = state.state.kind === "resolved" ? state.state.container_path : "";
  const [view, setView] = useState<View>({ kind: "loading" });
  const [language, setLanguage] = useState<Extension | null>(null);
  const [doc, dispatch] = useReducer(reduceViewer, initialViewerState);
  const [closing, setClosing] = useState(false);
  const [saveError, setSaveError] = useState<SaveError | null>(null);
  const editor = useRef<CodeEditorHandle>(null);
  const docRef = useRef(doc);
  docRef.current = doc;
  const closingRef = useRef(closing);
  closingRef.current = closing;
  /** Bumped synchronously on every user edit, so async work can tell an edit happened meanwhile. */
  const editGen = useRef(0);
  const saving = useRef(false);
  /** Bumped when a save's write settles; a poll issued before that is stale. */
  const saveGen = useRef(0);
  /** Line ending and BOM of the loaded text, restored on save. */
  const textFormat = useRef<TextFormat>({ bom: false, eol: "\n" });
  const imageUrl = useRef<string | null>(null);

  const markEdited = useCallback(() => {
    editGen.current += 1;
    dispatch({ type: "edited" });
  }, []);

  /** Put a freshly read file on screen: text into the editor, or an image/binary view. */
  const show = useCallback((file: ViewerFile) => {
    const bytes = decodeBase64(file.contents_base64);
    const classified = classifyViewerFile(path, file, bytes);
    if (imageUrl.current) { URL.revokeObjectURL(imageUrl.current); imageUrl.current = null; }
    if (classified.kind === "image") {
      const url = URL.createObjectURL(new Blob([bytes], { type: imageMimeFor(path) ?? "application/octet-stream" }));
      imageUrl.current = url;
      setView({ kind: "image", url, editability: classified });
    } else if (classified.kind === "binary") {
      setView({ kind: "binary", editability: classified });
    } else {
      const { text, editability, format } = decodeViewerText(bytes, classified);
      textFormat.current = format;
      setView({ kind: "text", doc: text, editability });
      editor.current?.setDoc(text);
    }
  }, [path]);

  useEffect(() => () => { if (imageUrl.current) URL.revokeObjectURL(imageUrl.current); }, []);

  /** Bumped per initial-load attempt (and on unmount/path change); a stale attempt's result is dropped. */
  const loadGen = useRef(0);

  /**
   * The initial read. Re-run by "Retry" and by the poll while the window shows
   * a load error, so a window opened while the container was restarting
   * recovers on its own instead of staying dead.
   */
  const load = useCallback(async () => {
    const gen = ++loadGen.current;
    try {
      const file = await viewerReadFile(previewLimit(path));
      if (loadGen.current !== gen) return;
      show(file);
      dispatch({ type: "loaded", hash: file.hash, truncated: file.truncated });
    } catch (e) {
      if (loadGen.current === gen) setView({ kind: "error", message: errorText(e) });
    }
  }, [path, show]);

  useEffect(() => {
    void load();
    return () => { loadGen.current += 1; };
  }, [load]);

  const retryLoad = useCallback(() => {
    setView({ kind: "loading" });
    void load();
  }, [load]);

  // The language loads lazily and separately, so the text is on screen (and
  // polling runs) without waiting for a grammar chunk.
  useEffect(() => {
    let cancelled = false;
    languageFor(path).then((l) => { if (!cancelled) setLanguage(l); }, () => {});
    return () => { cancelled = true; };
  }, [path]);

  /**
   * The one reload path (P3/P14), for a clean poll-driven reload and for
   * "Reload (discard mine)". `polledHash` is the poll's full-file hash, which
   * a truncated read's own (prefix) hash can never equal. With `onlyIfClean`,
   * an edit made while the read was in flight wins: nothing is replaced, and
   * the next poll shows the banner instead.
   */
  const reloadFromDisk = useCallback(async (polledHash: string | null, onlyIfClean: boolean) => {
    const gen = editGen.current;
    const file = await viewerReadFile(previewLimit(path));
    if (onlyIfClean && editGen.current !== gen) return;
    show(file);
    dispatch({ type: "reloaded", hash: file.hash, truncated: file.truncated, polledHash });
  }, [path, show]);

  // Poll (spec §5). A reload replaces the document only when the reducer says so.
  // While the first read has failed, each tick retries that read instead.
  // Always enabled, so a loading -> error flip does not fire an immediate extra read.
  useViewerPolling(POLL_MS, async () => {
    if (view.kind === "loading") return;
    if (view.kind === "error") { await load(); return; }
    // A poll that overlaps a save can carry the pre-save hash; skip it (M2).
    if (saving.current) return;
    const gen = saveGen.current;
    let poll;
    try {
      poll = await viewerPollFile();
    } catch (e) {
      if (saveGen.current === gen) dispatch({ type: "poll_failed", message: errorText(e) });
      return;
    }
    if (saveGen.current !== gen) return;
    const before = docRef.current;
    const after = reduceViewer(before, { type: "polled", poll });
    dispatch({ type: "polled", poll });
    if (pollEffect(before, after) === "reload") {
      try { await reloadFromDisk(after.diskHash, true); } catch (e) { dispatch({ type: "poll_failed", message: errorText(e) }); }
    }
  }, true);

  const editable = view.kind === "text" && view.editability.editable;
  const saveEnabled = canSave(doc, editable);

  const save = useCallback(async () => {
    const handle = editor.current;
    const baseHash = docRef.current.baseHash;
    if (!saveEnabled || !handle || !baseHash || saving.current) return;
    saving.current = true;
    setSaveError(null);
    const gen = editGen.current;
    try {
      const bytes = encodeViewerText(handle.getDoc(), textFormat.current);
      const result = await viewerWriteFile(encodeBase64(bytes), baseHash).then(
        (saved) => ({ ok: true as const, saved }),
        (e: unknown) => ({ ok: false as const, msg: errorText(e) }),
      );
      saveGen.current += 1;
      if (result.ok) {
        const { hash, disk_hash: diskHash } = result.saved;
        dispatch({ type: "saved", hash, diskHash });
        if (editGen.current !== gen) dispatch({ type: "edited" }); // typed while the save was in flight
        // Another writer landed right after ours: the reducer shows "Changed on
        // disk", and the window stays open so the user can decide.
        else if (closingRef.current && diskHash === hash) await getCurrentWindow().destroy();
      } else if (result.msg.startsWith(CONFLICT_PREFIX)) {
        await adoptConflict();
      } else if (result.msg.startsWith(GONE_PREFIX)) {
        dispatch({ type: "save_gone" });
      } else if (isReadOnlyRefusal(result.msg)) {
        setSaveError({ text: READ_ONLY_SAVE });
      } else {
        setSaveError({ text: result.msg });
      }
    } finally {
      saving.current = false;
    }
  }, [saveEnabled]);

  /**
   * The disk changed between polls. Poll now (P4), so "Overwrite on save"
   * adopts the current hash rather than the stale one. With no hash to adopt,
   * an overwrite would only conflict again, so say so instead (M3).
   */
  async function adoptConflict() {
    let poll;
    try {
      poll = await viewerPollFile();
    } catch (e) {
      dispatch({ type: "poll_failed", message: errorText(e) });
      dispatch({ type: "save_conflict" });
      return;
    }
    if (poll.exists && poll.hash === null) { setSaveError({ text: CONFLICT_UNCHECKED, reload: true }); return; }
    dispatch({ type: "polled", poll });
    if (poll.exists) dispatch({ type: "save_conflict" });
  }

  // Ctrl/Cmd+S outside the editor; the editor's own keymap handles it inside
  // (and prevents the default, which is how this listener knows to skip it).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || !(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "s") return;
      e.preventDefault();
      void save();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [save]);

  // Close guard + goto (spec §3/§5).
  useEffect(() => {
    const win = getCurrentWindow();
    let disposed = false;
    const unlisten: Array<() => void> = [];
    const keep = (u: () => void) => { if (disposed) u(); else unlisten.push(u); };
    void win.onCloseRequested((event) => {
      if (docRef.current.doc === "dirty") { event.preventDefault(); setClosing(true); }
    }).then(keep);
    void win.listen<ViewerLocation>(GOTO_EVENT, (e) => editor.current?.goTo(e.payload)).then(keep);
    return () => { disposed = true; unlisten.forEach((u) => u()); };
  }, []);

  useEffect(() => {
    if (import.meta.env.MODE !== "test") return;
    document.addEventListener("triple-c-test-edit", markEdited);
    return () => document.removeEventListener("triple-c-test-edit", markEdited);
  }, [markEdited]);

  const reloadDiscarding = useCallback(async () => {
    setSaveError(null);
    try { await reloadFromDisk(docRef.current.diskHash, false); } catch (e) { setSaveError({ text: errorText(e) }); }
  }, [reloadFromDisk]);

  const badge = useMemo((): { tone: StatusTone; label: string; detail?: string } | null => {
    if (view.kind === "loading" || view.kind === "error") return null;
    if (doc.containerDown) return { tone: "error", label: "Container not running" };
    if (doc.pollError) return { tone: "error", label: "Could not check for changes" };
    if (doc.disk === "gone") return { tone: "error", label: "File no longer exists" };
    if (!view.editability.editable) return { tone: "off", label: "Read-only", detail: view.editability.reason ?? undefined };
    if (doc.disk === "changed") return { tone: "busy", label: "Changed on disk" };
    if (doc.doc === "dirty") return { tone: "busy", label: "Unsaved" };
    if (doc.justReloaded) return { tone: "ok", label: "Reloaded" };
    return { tone: "ok", label: "Saved" };
  }, [doc, view]);

  return (
    <div className="flex h-screen flex-col bg-[var(--bg-primary)] text-[var(--text-primary)]">
      <header className="flex items-center gap-3 border-b border-[var(--border-color)] bg-[var(--bg-secondary)] px-3 py-2 text-xs">
        <span className="truncate font-mono" title={path}>{path}</span>
        <span className="text-[var(--text-secondary)]">{state.project_name}</span>
        <span className="ml-auto flex items-center" aria-live="polite">
          {badge && <StatusIndicator tone={badge.tone} label={badge.label} />}
          {badge?.detail && <span className="ml-2 text-[var(--text-secondary)]">{badge.detail}</span>}
        </span>
        <Button variant="primary" size="sm" onClick={() => void save()} disabled={!saveEnabled}>Save</Button>
      </header>

      {doc.containerDown && <Banner tone="error" text="Container not running — the file cannot be read or saved until the project starts again." />}
      {doc.pollError && <Banner tone="error" text={`${doc.pollError} — changes on disk go undetected until this clears; the viewer keeps trying.`} />}
      {doc.disk === "gone" && <Banner tone="error" text="This file no longer exists in the container. Your text is kept so you can copy it; saving is disabled." />}
      {doc.disk === "changed" && doc.doc === "dirty" && (
        <Banner text="Changed on disk while you were editing.">
          <Button size="sm" onClick={() => void reloadDiscarding()}>Reload (discard mine)</Button>
          <Button size="sm" onClick={() => dispatch({ type: "overwrite_on_save" })}>Overwrite on save</Button>
        </Banner>
      )}
      {saveError && (
        <Banner tone="error" text={saveError.text}>
          {saveError.reload && <Button size="sm" onClick={() => void reloadDiscarding()}>Reload (discard mine)</Button>}
        </Banner>
      )}
      {closing && (
        <Banner text="Unsaved changes — save before closing?">
          <Button variant="primary" size="sm" onClick={() => void save()} disabled={!saveEnabled}>Save and close</Button>
          <Button variant="danger" size="sm" onClick={() => void getCurrentWindow().destroy()}>Discard</Button>
          <Button size="sm" onClick={() => setClosing(false)}>Cancel</Button>
        </Banner>
      )}

      <main className="min-h-0 flex-1">
        {view.kind === "loading" && <p className="p-4 text-sm text-[var(--text-secondary)]">Loading…</p>}
        {view.kind === "error" && (
          <div className="flex flex-col items-start gap-2 p-4 text-sm">
            <p>{view.message}</p>
            <p className="text-[var(--text-secondary)]">The viewer retries every few seconds.</p>
            <Button size="sm" onClick={retryLoad}>Retry</Button>
          </div>
        )}
        {view.kind === "binary" && <p className="p-4 text-sm">{view.editability.reason}</p>}
        {view.kind === "image" && <img src={view.url} alt={path} className="max-h-full max-w-full object-contain p-4" />}
        {view.kind === "text" && (
          <CodeEditor
            ref={editor}
            initialDoc={view.doc}
            readOnly={!view.editability.editable}
            language={language}
            lineWrapping={wrapsLines(path)}
            initialLocation={state.initial}
            onDocChanged={markEdited}
            onSave={() => void save()}
          />
        )}
      </main>
    </div>
  );
}

/** A warning is a polite status; an error (a failed save, a lost file or container) is an alert. */
function Banner({ text, tone = "warning", children }: { text: string; tone?: "warning" | "error"; children?: ReactNode }) {
  const colours = tone === "error"
    ? "border-[var(--error)] bg-[var(--error-muted)]"
    : "border-[var(--warning)] bg-[var(--warning-muted)]";
  return (
    <div role={tone === "error" ? "alert" : "status"} className={`flex flex-wrap items-center gap-2 border-b px-3 py-2 text-xs ${colours}`}>
      <span>{text}</span>
      {children}
    </div>
  );
}
