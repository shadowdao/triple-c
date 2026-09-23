import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import { EditorView } from "@codemirror/view";
import EditorPane from "./EditorPane";
import { encodeBase64 } from "../components/projects/home/filePreview";
import type { ViewerState } from "../lib/types";

const H1 = "1".repeat(64);
const H2 = "2".repeat(64);
const H3 = "3".repeat(64);
const b64 = (s: string) => btoa(s);

const commands = vi.hoisted(() => ({
  viewerReadFile: vi.fn(),
  viewerPollFile: vi.fn(),
  viewerWriteFile: vi.fn(),
}));
vi.mock("../lib/tauri-commands", () => commands);

const windowApi = vi.hoisted(() => ({ closeRequested: null as null | ((e: { preventDefault(): void }) => Promise<void> | void), destroy: vi.fn(), listeners: new Map<string, (e: { payload: unknown }) => void>() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: async (cb: typeof windowApi.closeRequested) => { windowApi.closeRequested = cb; return () => {}; },
    listen: async (name: string, cb: (e: { payload: unknown }) => void) => { windowApi.listeners.set(name, cb); return () => {}; },
    destroy: windowApi.destroy,
  }),
}));

const state: ViewerState = {
  project_id: "p", project_name: "Demo", raw_path: "notes.md",
  state: { kind: "resolved", container_path: "/workspace/demo/notes.md" },
  initial: { line: 1, col: null, end_line: null },
};

const textFile = (text: string, hash: string, extra: Partial<{ truncated: boolean; editable: boolean }> = {}) => ({
  contents_base64: b64(text), truncated: false, size: text.length, hash, editable: true, readonly_reason: null, ...extra,
});

/** Mark the buffer dirty through the pane's test hook (jsdom cannot drive CodeMirror's contenteditable). */
const edit = () => fireEvent(document, new CustomEvent("triple-c-test-edit"));
const clickSave = async () => { await act(async () => { fireEvent.click(screen.getByRole("button", { name: /^save$/i })); }); };
/** A real edit through CodeMirror, so the saved bytes carry it. */
const typeInto = (from: number, to: number, insert: string) => {
  const view = EditorView.findFromDOM(document.querySelector(".cm-editor") as HTMLElement);
  if (!view) throw new Error("no editor");
  act(() => { view.dispatch({ changes: { from, to, insert } }); });
};
const bytesB64 = (bytes: number[]) => encodeBase64(new Uint8Array(bytes));
const utf8 = (s: string) => Array.from(new TextEncoder().encode(s));
const READ_ONLY = "Could not save the file: The file is read-only for the container user.";
const NOT_RUNNING = "Start the project before checking this file for changes — it runs inside the running container.";
const saved = (hash: string, diskHash = hash) => ({ hash, disk_hash: diskHash });
const poll = async (ms = 2100) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

describe("EditorPane", () => {
  beforeAll(() => {
    // P17: CodeMirror's measure pass calls Range geometry, which jsdom lacks.
    const rect = () => ({ x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0, toJSON() {} }) as DOMRect;
    Range.prototype.getClientRects = () => ({ length: 0, item: () => null, [Symbol.iterator]: [][Symbol.iterator] }) as unknown as DOMRectList;
    Range.prototype.getBoundingClientRect = rect;
  });

  beforeEach(() => {
    // Only the poll's interval is faked. Testing Library's async utilities
    // settle through a real setTimeout(0), which fully faked timers freeze.
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    commands.viewerReadFile.mockReset().mockResolvedValue(textFile("hello\n", H1));
    commands.viewerPollFile.mockReset().mockResolvedValue({ exists: true, hash: H1, size: 6 });
    commands.viewerWriteFile.mockReset().mockResolvedValue(saved(H2));
    windowApi.destroy.mockReset();
  });
  afterEach(() => vi.useRealTimers());

  it("loads the file and shows the path", async () => {
    render(<EditorPane state={state} />);
    expect(await screen.findByText("/workspace/demo/notes.md")).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledWith(1024 * 1024);
    expect(await screen.findByText("Saved")).toBeInTheDocument();
  });

  it("a changed poll on a clean document reloads silently", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H2, size: 8 });
    commands.viewerReadFile.mockResolvedValue(textFile("changed\n", H2));
    await poll();
    expect(await screen.findByText(/Reloaded/)).toBeInTheDocument();
    expect(screen.queryByText(/while you were editing/)).toBeNull();
    expect(screen.getByTestId("code-editor")).toHaveTextContent("changed");
  });

  it("a changed poll on a dirty document shows the banner instead of reloading", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H2, size: 8 });
    await poll();
    expect(await screen.findByText(/while you were editing/)).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(1);
  });

  it("a truncated file reloads once per change, not on every poll", async () => {
    commands.viewerReadFile.mockResolvedValue(textFile("big", "a".repeat(64), { truncated: true }));
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    await poll(); // seeds diskHash = H1 from the poll
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H2, size: 9 });
    commands.viewerReadFile.mockResolvedValue(textFile("bigger", "b".repeat(64), { truncated: true }));
    await poll();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(2);
    await poll(2000);
    await poll(2000);
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(2);
  });

  it("a gone file shows the banner and disables Save", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    expect(screen.getByRole("button", { name: /^save$/i })).toBeEnabled();
    commands.viewerPollFile.mockResolvedValue({ exists: false, hash: null, size: null });
    await poll();
    expect(await screen.findByText(/in the container/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^save$/i })).toBeDisabled();
  });

  it("saves the buffer against the loaded hash", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    await clickSave();
    expect(commands.viewerWriteFile).toHaveBeenCalledWith(b64("hello\n"), H1);
    expect(await screen.findByText("Saved")).toBeInTheDocument();
  });

  it("a save conflict shows the Changed on disk banner with both choices", async () => {
    commands.viewerWriteFile.mockRejectedValue(new Error("conflict: the file changed on disk since it was loaded."));
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    await clickSave();
    expect(await screen.findByText(/while you were editing/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Reload/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Overwrite on save/ })).toBeInTheDocument();
  });

  it("after a conflict, Overwrite on save saves against the freshly polled hash", async () => {
    // A string rejection, as Tauri's invoke delivers it.
    commands.viewerWriteFile.mockRejectedValueOnce("conflict: the file changed on disk since it was loaded.");
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H3, size: 7 });
    await clickSave();
    const overwrite = await screen.findByRole("button", { name: /Overwrite on save/ });
    await act(async () => { fireEvent.click(overwrite); });
    commands.viewerWriteFile.mockResolvedValue(saved(H2));
    await clickSave();
    expect(commands.viewerWriteFile).toHaveBeenLastCalledWith(b64("hello\n"), H3);
    expect(await screen.findByText("Saved")).toBeInTheDocument();
  });

  it("Reload (discard mine) replaces the buffer with the disk copy", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H2, size: 8 });
    commands.viewerReadFile.mockResolvedValue(textFile("theirs\n", H2));
    await poll();
    const reload = await screen.findByRole("button", { name: /Reload/ });
    await act(async () => { fireEvent.click(reload); });
    expect(screen.queryByText(/while you were editing/)).toBeNull();
    expect(screen.getByTestId("code-editor")).toHaveTextContent("theirs");
    expect(screen.getByRole("button", { name: /^save$/i })).toBeDisabled();
  });

  it("a save refused because the file is read-only says so and keeps the buffer", async () => {
    commands.viewerWriteFile.mockRejectedValue(READ_ONLY);
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    await clickSave();
    expect(await screen.findByText(/read-only for the container user/)).toBeInTheDocument();
    expect(screen.getByTestId("code-editor")).toHaveTextContent("hello");
    expect(screen.getByText("Unsaved")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^save$/i })).toBeEnabled();
  });

  it("any other save failure is shown as it came", async () => {
    commands.viewerWriteFile.mockRejectedValue("Could not save the file: disk full");
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    await clickSave();
    expect(await screen.findByText("Could not save the file: disk full")).toBeInTheDocument();
  });

  it("a poll refused because the container is down shows Container not running and disables Save", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    commands.viewerPollFile.mockRejectedValue(NOT_RUNNING);
    await poll();
    expect(await screen.findByText(/until the project starts again/)).toBeInTheDocument();
    expect(screen.getByText("Container not running")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^save$/i })).toBeDisabled();
  });

  it("any other poll failure says what failed, not that the container is down, and clears on a good poll", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    commands.viewerPollFile.mockRejectedValue("Could not check the file: sha256sum: Permission denied");
    await poll();
    expect(await screen.findByRole("alert")).toHaveTextContent(/Could not check the file: sha256sum: Permission denied/);
    expect(screen.getByText("Could not check for changes")).toBeInTheDocument();
    expect(screen.queryByText(/Container not running/)).toBeNull();
    // The write re-checks the hash itself, so saving stays possible.
    expect(screen.getByRole("button", { name: /^save$/i })).toBeEnabled();
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H1, size: 6 });
    await poll(2000);
    expect(screen.queryByText(/Permission denied/)).toBeNull();
    expect(screen.getByText("Unsaved")).toBeInTheDocument();
  });

  it("a save that another writer overtook shows Changed on disk instead of Saved", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    commands.viewerWriteFile.mockResolvedValue(saved(H2, H3));
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H3, size: 7 });
    await clickSave();
    expect(await screen.findByText(/while you were editing/)).toBeInTheDocument();
    expect(screen.getByText("Changed on disk")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^save$/i })).toBeDisabled();
    // The next poll sees the same foreign hash: the banner stays, nothing is reloaded over the buffer.
    await poll();
    expect(screen.getByText(/while you were editing/)).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(1);
    // Overwrite now saves against what is actually on disk.
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /Overwrite on save/ })); });
    commands.viewerWriteFile.mockResolvedValue(saved(H2));
    await clickSave();
    expect(commands.viewerWriteFile).toHaveBeenLastCalledWith(b64("hello\n"), H3);
    expect(await screen.findByText("Saved")).toBeInTheDocument();
  });

  it("Save and close does not close when another writer overtook the save", async () => {
    commands.viewerWriteFile.mockResolvedValue(saved(H2, H3));
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    await act(async () => { await windowApi.closeRequested?.({ preventDefault: () => {} }); });
    await act(async () => { fireEvent.click(await screen.findByRole("button", { name: "Save and close" })); });
    expect(windowApi.destroy).not.toHaveBeenCalled();
    expect(await screen.findByText(/while you were editing/)).toBeInTheDocument();
  });

  it("a failed first read offers Retry, which loads the file", async () => {
    commands.viewerReadFile.mockRejectedValueOnce(NOT_RUNNING.replace("checking this file for changes", "opening files"));
    render(<EditorPane state={state} />);
    expect(await screen.findByText(/Start the project before opening files/)).toBeInTheDocument();
    const retry = screen.getByRole("button", { name: "Retry" });
    await act(async () => { fireEvent.click(retry); });
    expect(await screen.findByText("Saved")).toBeInTheDocument();
    expect(screen.getByTestId("code-editor")).toHaveTextContent("hello");
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("a failed first read is retried by the poll until it succeeds", async () => {
    commands.viewerReadFile.mockRejectedValueOnce("Docker is busy").mockRejectedValueOnce("Docker is still busy");
    render(<EditorPane state={state} />);
    expect(await screen.findByText("Docker is busy")).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(1);
    await poll();
    expect(await screen.findByText("Docker is still busy")).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(2);
    expect(commands.viewerPollFile).not.toHaveBeenCalled();
    await poll(2000);
    expect(await screen.findByText("Saved")).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(3);
    // Loaded: the poll is back to polling, not re-reading.
    await poll(2000);
    expect(commands.viewerPollFile).toHaveBeenCalled();
    expect(commands.viewerReadFile).toHaveBeenCalledTimes(3);
  });

  it("closing with unsaved edits is intercepted", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    edit();
    const prevent = vi.fn();
    await act(async () => { await windowApi.closeRequested?.({ preventDefault: prevent }); });
    expect(prevent).toHaveBeenCalled();
    expect(await screen.findByText(/Unsaved changes/)).toBeInTheDocument();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /Discard/ })); });
    expect(windowApi.destroy).toHaveBeenCalled();
  });

  it("closing a clean document is not intercepted", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    const prevent = vi.fn();
    await act(async () => { await windowApi.closeRequested?.({ preventDefault: prevent }); });
    expect(prevent).not.toHaveBeenCalled();
    expect(screen.queryByText(/Unsaved changes/)).toBeNull();
  });

  it("a one-character edit to a CRLF file saves with every CRLF intact", async () => {
    commands.viewerReadFile.mockResolvedValue(textFile("a\r\nb\r\nc\r\n", H1));
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    typeInto(2, 3, "B"); // the editor holds "a\nb\nc\n"
    expect(screen.getByText("Unsaved")).toBeInTheDocument();
    await clickSave();
    expect(commands.viewerWriteFile).toHaveBeenCalledWith(b64("a\r\nB\r\nc\r\n"), H1);
  });

  it("a file with a UTF-8 BOM keeps its BOM on save", async () => {
    const BOM = [0xef, 0xbb, 0xbf];
    commands.viewerReadFile.mockResolvedValue({ ...textFile("", H1), contents_base64: bytesB64([...BOM, ...utf8("hi\n")]) });
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    typeInto(2, 2, "!");
    await clickSave();
    expect(commands.viewerWriteFile).toHaveBeenCalledWith(bytesB64([...BOM, ...utf8("hi!\n")]), H1);
  });

  it("a reload that fails is retried on the next poll", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H2, size: 8 });
    commands.viewerReadFile.mockRejectedValueOnce("Could not read the file: I/O error").mockResolvedValue(textFile("changed\n", H2));
    await poll();
    expect(screen.getByTestId("code-editor")).toHaveTextContent("hello");
    await poll(2000);
    expect(screen.getByTestId("code-editor")).toHaveTextContent("changed");
    expect(screen.getByText("Reloaded")).toBeInTheDocument();
  });

  it("ignores a poll issued before a save completed", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    let answer: (p: { exists: boolean; hash: string; size: number }) => void = () => {};
    commands.viewerPollFile.mockImplementationOnce(() => new Promise((r) => { answer = r; }));
    await poll(); // this poll is now in flight, carrying the pre-save hash
    await clickSave(); // lands as H2
    edit();
    await act(async () => { answer({ exists: true, hash: H1, size: 6 }); });
    expect(screen.queryByText(/while you were editing/)).toBeNull();
    expect(screen.getByText("Unsaved")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^save$/i })).toBeEnabled();
  });

  it("a conflict whose follow-up poll has no hash shows an error instead of offering an overwrite", async () => {
    commands.viewerWriteFile.mockRejectedValue("conflict: the file changed on disk since it was loaded.");
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: null, size: 7 });
    await clickSave();
    expect(await screen.findByRole("alert")).toHaveTextContent(/could not be checked/);
    expect(screen.queryByRole("button", { name: /Overwrite on save/ })).toBeNull();
    expect(screen.getByRole("button", { name: /Reload/ })).toBeInTheDocument();
  });

  it("states why a file is read-only as visible text", async () => {
    commands.viewerReadFile.mockResolvedValue(textFile("big", H1, { truncated: true }));
    render(<EditorPane state={state} />);
    expect(await screen.findByText("Read-only")).toBeInTheDocument();
    expect(screen.getByText("Files over 1 MiB are read-only.")).toBeVisible();
  });

  it("a save error is announced as an alert", async () => {
    commands.viewerWriteFile.mockRejectedValue("Could not save the file: disk full");
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    await clickSave();
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not save the file: disk full");
  });

  it("Save and close saves, then closes the window", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    await act(async () => { await windowApi.closeRequested?.({ preventDefault: () => {} }); });
    await act(async () => { fireEvent.click(await screen.findByRole("button", { name: "Save and close" })); });
    expect(commands.viewerWriteFile).toHaveBeenCalledWith(b64("hello\n"), H1);
    expect(windowApi.destroy).toHaveBeenCalled();
  });

  it("a save that fails while closing keeps the window open and the buffer", async () => {
    commands.viewerWriteFile.mockRejectedValue(READ_ONLY);
    render(<EditorPane state={state} />);
    await screen.findByText("Saved");
    edit();
    await act(async () => { await windowApi.closeRequested?.({ preventDefault: () => {} }); });
    await act(async () => { fireEvent.click(await screen.findByRole("button", { name: "Save and close" })); });
    expect(windowApi.destroy).not.toHaveBeenCalled();
    expect(screen.getByText(/Unsaved changes/)).toBeInTheDocument();
    expect(await screen.findByText(/read-only for the container user/)).toBeInTheDocument();
    expect(screen.getByTestId("code-editor")).toHaveTextContent("hello");
    expect(screen.getByText("Unsaved")).toBeInTheDocument();
  });
});
