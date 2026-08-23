import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import FilesTab from "./FilesTab";
import type { FileContents, FileEntry, Project } from "../../../lib/types";

const listContainerFiles = vi.fn();
const downloadContainerFile = vi.fn(async () => {});
const uploadFileToContainer = vi.fn(async () => {});
const renameContainerPath = vi.fn(async () => "");
const createContainerDirectory = vi.fn(async () => "");
const readContainerFile = vi.fn();

vi.mock("../../../lib/tauri-commands", () => ({
  listContainerFiles: (p: string, path: string) => listContainerFiles(p, path),
  downloadContainerFile: (p: string, c: string, h: string) => downloadContainerFile(p, c, h),
  uploadFileToContainer: (...args: unknown[]) => uploadFileToContainer(...args),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
  readContainerFile: (p: string, path: string, max?: number) => readContainerFile(p, path, max),
}));

/** Transient failures land in `ToastHost`, not in an inline string. */
const pushToast = vi.fn();
vi.mock("../../../store/appState", () => ({
  useAppState: { getState: () => ({ pushToast }) },
}));

const toastText = () =>
  pushToast.mock.calls
    .map(([toast]) => `${toast.kind}: ${toast.message} ${toast.detail ?? ""}`)
    .join("\n");

const save = vi.fn(async () => "/host/out");
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: (o: unknown) => save(o),
  open: vi.fn(async () => null),
}));

/** The webview's window-wide native drag-drop listener, captured for driving. */
type DragPayload =
  | { type: "enter" | "over"; position: { x: number; y: number }; paths: string[] }
  | { type: "leave" }
  | { type: "drop"; position: { x: number; y: number }; paths: string[] };
let dragHandler: ((e: { payload: DragPayload }) => void | Promise<void>) | null = null;
const unlistenDrag = vi.fn();

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (cb: (e: { payload: DragPayload }) => void) => {
      dragHandler = cb;
      return unlistenDrag;
    },
  }),
}));

const project = { id: "p1", name: "api", status: "running" } as unknown as Project;

const entry = (name: string, extra: Partial<FileEntry> = {}): FileEntry => ({
  name,
  path: `/workspace/${name}`,
  is_directory: false,
  is_symlink: false,
  size: 12,
  modified: "2024-05-01 10:00:00",
  permissions: "644",
  ...extra,
});

const contents = (text: string, extra: Partial<FileContents> = {}): FileContents => ({
  contents_base64: btoa(text),
  truncated: false,
  size: text.length,
  ...extra,
});

async function renderTab() {
  const view = render(<FilesTab project={project} />);
  await act(async () => {
    await Promise.resolve();
  });
  return view;
}

/** Fire the native drop payload at a point inside the pane's stubbed rect. */
async function drop(paths: string[], position = { x: 100, y: 100 }) {
  await act(async () => {
    await dragHandler?.({ payload: { type: "drop", position, paths } });
  });
}

/** Every row that is part of the grid's roving tabindex, in order. */
const gridRows = () => Array.from(document.querySelectorAll("tr[data-file-row]"));
/** The rows that are actually tab stops. There must never be more than one. */
const tabStops = () => gridRows().filter((r) => r.getAttribute("tabindex") === "0");

/** Fire a drop without awaiting it — for the paths that stop to ask a question. */
function dropWithoutWaiting(paths: string[], position = { x: 100, y: 100 }) {
  let pending: unknown;
  act(() => {
    pending = dragHandler?.({ payload: { type: "drop", position, paths } });
  });
  return pending as Promise<void> | undefined;
}

beforeEach(() => {
  vi.clearAllMocks();
  dragHandler = null;
  listContainerFiles.mockResolvedValue([
    entry("src", { is_directory: true, path: "/workspace/src" }),
    entry("notes.txt"),
  ]);
  // jsdom lays nothing out, so the pane's hit-test rect has to be supplied.
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0, y: 0, left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600,
    toJSON: () => ({}),
  } as DOMRect);
  // Not implemented in jsdom; the image preview needs both halves.
  URL.createObjectURL = vi.fn(() => "blob:mock-url");
  URL.revokeObjectURL = vi.fn();
});

describe("FilesTab listing", () => {
  it("lists /workspace once the container is running", async () => {
    await renderTab();
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace");
    expect(screen.getByText("notes.txt")).toBeTruthy();
  });

  it("says nothing about files while the container is stopped", async () => {
    render(<FilesTab project={{ ...project, status: "stopped" } as Project} />);
    expect(screen.getByText(/Start the container/)).toBeTruthy();
    expect(listContainerFiles).not.toHaveBeenCalled();
  });

  it("labels a symlink, which no longer masquerades as a plain file", async () => {
    listContainerFiles.mockResolvedValue([
      entry("app", { is_directory: true, is_symlink: true }),
    ]);
    await renderTab();
    expect(screen.getByTitle("Symbolic link")).toBeTruthy();
  });
});

describe("FilesTab open semantics", () => {
  it("selects on a single click without navigating", async () => {
    await renderTab();
    listContainerFiles.mockClear();
    fireEvent.click(screen.getByText("src"));
    expect(listContainerFiles).not.toHaveBeenCalled();
    expect(screen.getByText("src").closest("tr")?.getAttribute("aria-selected")).toBe("true");
  });

  it("navigates a directory on double click", async () => {
    await renderTab();
    listContainerFiles.mockClear();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("src"));
    });
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace/src");
  });

  it("walks the rows with the arrow keys, which is what makes the grid role honest", async () => {
    await renderTab();
    const first = screen.getByText("src").closest("tr")!;
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByText("notes.txt").closest("tr"));
    fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
    expect(document.activeElement).toBe(first);
  });

  it("opens a directory from the keyboard with Enter", async () => {
    await renderTab();
    listContainerFiles.mockClear();
    const row = screen.getByText("src").closest("tr")!;
    // Not a tab stop — `..` holds the grid's single one until the arrows move
    // it — but still focusable and still openable from the keyboard.
    expect(row.getAttribute("tabindex")).toBe("-1");
    await act(async () => {
      fireEvent.keyDown(row, { key: "Enter" });
    });
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace/src");
  });
});

describe("FilesTab viewer", () => {
  it("shows a text file's contents in a dialog", async () => {
    readContainerFile.mockResolvedValue(contents("hello from the container"));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("notes.txt"));
    });
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toBeTruthy();
    expect(await screen.findByText("hello from the container")).toBeTruthy();
    // A text file gets the text-sized budget, not the image one.
    expect(readContainerFile).toHaveBeenCalledWith("p1", "/workspace/notes.txt", 1024 * 1024);
  });

  it("renders an image through a revocable blob URL, not a data URI", async () => {
    // `data:` is absent from the app's img-src on purpose; `blob:` is what was
    // added, and the object URL has to be released when the dialog closes.
    listContainerFiles.mockResolvedValue([entry("logo.png", { size: 4 })]);
    readContainerFile.mockResolvedValue(contents("\x89PNG"));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("logo.png"));
    });
    const img = (await screen.findByAltText("logo.png")) as HTMLImageElement;
    expect(img.getAttribute("src")).toBe("blob:mock-url");
    expect(readContainerFile).toHaveBeenCalledWith("p1", "/workspace/logo.png", 5 * 1024 * 1024);

    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:mock-url"));
  });

  it("refuses an oversized image rather than drawing a half-decoded one", async () => {
    listContainerFiles.mockResolvedValue([entry("huge.png", { size: 40 * 1024 * 1024 })]);
    readContainerFile.mockResolvedValue(contents("\x89PNG", { truncated: true, size: 40 * 1024 * 1024 }));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("huge.png"));
    });
    expect(await screen.findByText(/too large to preview/)).toBeTruthy();
    expect(screen.queryByAltText("huge.png")).toBeNull();
    expect(screen.getByRole("button", { name: "Save to host…" })).toBeTruthy();
  });

  it("says so in words when only a prefix of a big text file came back", async () => {
    readContainerFile.mockResolvedValue(
      contents("first megabyte", { truncated: true, size: 5 * 1024 * 1024 }),
    );
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("notes.txt"));
    });
    expect(await screen.findByText(/Showing the first/)).toBeTruthy();
    expect(screen.getByText("first megabyte")).toBeTruthy();
  });

  it("offers Save to host for a file it cannot render", async () => {
    listContainerFiles.mockResolvedValue([entry("blob.bin")]);
    readContainerFile.mockResolvedValue(contents("a\x00b"));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("blob.bin"));
    });
    expect(await screen.findByText(/no preview for this file type/)).toBeTruthy();
  });
});

describe("FilesTab rename", () => {
  it("commits an inline rename on Enter and re-lists", async () => {
    await renderTab();
    fireEvent.click(screen.getByRole("button", { name: "Rename — notes.txt" }));
    const input = screen.getByLabelText("New name for notes.txt") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "renamed.txt" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
      fireEvent.blur(input);
    });
    expect(renameContainerPath).toHaveBeenCalledWith("p1", "/workspace/notes.txt", "renamed.txt");
  });

  it("abandons the rename on Escape", async () => {
    await renderTab();
    fireEvent.click(screen.getByRole("button", { name: "Rename — notes.txt" }));
    const input = screen.getByLabelText("New name for notes.txt");
    fireEvent.change(input, { target: { value: "nope.txt" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Escape" });
    });
    expect(renameContainerPath).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("New name for notes.txt")).toBeNull();
  });

  it("starts a rename from the keyboard with F2", async () => {
    await renderTab();
    const row = screen.getByText("notes.txt").closest("tr")!;
    fireEvent.keyDown(row, { key: "F2" });
    expect(screen.getByLabelText("New name for notes.txt")).toBeTruthy();
  });

  it("reports a refused rename where it can be seen, not three hundred rows down", async () => {
    // The inline `error` div is the first child of the *scrolling* list, so
    // deep in a directory this used to be a rename box that stayed open and
    // said nothing. `ToastHost` is fixed, above the modal layer, and persists.
    renameContainerPath.mockRejectedValue("mv: cannot move '/etc/hosts': Permission denied");
    await renderTab();
    fireEvent.click(screen.getByRole("button", { name: "Rename — notes.txt" }));
    const input = screen.getByLabelText("New name for notes.txt");
    fireEvent.change(input, { target: { value: "x" } });
    await act(async () => {
      fireEvent.blur(input);
    });
    expect(toastText()).toContain("Permission denied");
    expect(screen.queryByRole("alert")).toBeNull();
    // The editor stays open, because the rename did not happen.
    expect(screen.getByLabelText("New name for notes.txt")).toBeTruthy();
  });
});

describe("FilesTab new folder", () => {
  it("creates a folder under the current directory", async () => {
    await renderTab();
    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    const input = screen.getByLabelText("New folder name");
    fireEvent.change(input, { target: { value: "assets" } });
    await act(async () => {
      fireEvent.blur(input);
    });
    expect(createContainerDirectory).toHaveBeenCalledWith("p1", "/workspace", "assets");
  });
});

describe("FilesTab host drag-and-drop", () => {
  it("uploads dropped paths into the directory on screen, then re-lists", async () => {
    await renderTab();
    listContainerFiles.mockClear();
    await drop(["/host/a.png", "/host/b.png"]);
    expect(uploadFileToContainer).toHaveBeenNthCalledWith(1, "p1", "/host/a.png", "/workspace");
    expect(uploadFileToContainer).toHaveBeenNthCalledWith(2, "p1", "/host/b.png", "/workspace");
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace");
  });

  it("drops into the directory the user has navigated to", async () => {
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("src"));
    });
    await drop(["/host/a.png"]);
    expect(uploadFileToContainer).toHaveBeenCalledWith("p1", "/host/a.png", "/workspace/src");
  });

  it("ignores a drop outside the pane — the listener is window-wide", async () => {
    // This is the whole routing discipline: the terminal's listener is live at
    // the same time, and only the hit-test keeps them apart.
    await renderTab();
    await drop(["/host/a.png"], { x: 5000, y: 5000 });
    expect(uploadFileToContainer).not.toHaveBeenCalled();
  });

  it("divides the payload position by devicePixelRatio on Windows only", async () => {
    // Only wry's WebView2 backend hands over *physical* pixels; the macOS and
    // GTK ones deliver logical points and `tauri-runtime-wry` does not rescale
    // them. At dpr 2 a physical (900, 900) is a CSS (450, 450) — inside the
    // 800x600 pane — but the same payload on a HiDPI Mac or Linux box really
    // is (900, 900) and belongs to nobody.
    const originalDpr = window.devicePixelRatio;
    const originalUa = window.navigator.userAgent;
    Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
    Object.defineProperty(window.navigator, "userAgent", {
      value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
      configurable: true,
    });
    await renderTab();
    await drop(["/host/a.png"], { x: 900, y: 900 });
    expect(uploadFileToContainer).toHaveBeenCalled();

    Object.defineProperty(window.navigator, "userAgent", {
      value: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
      configurable: true,
    });
    vi.mocked(uploadFileToContainer).mockClear();
    await drop(["/host/a.png"], { x: 900, y: 900 });
    expect(uploadFileToContainer).not.toHaveBeenCalled();
    // …and the *unhalved* point still lands, which is the half a HiDPI Mac
    // user was losing.
    await drop(["/host/a.png"], { x: 400, y: 300 });
    expect(uploadFileToContainer).toHaveBeenCalled();

    Object.defineProperty(window, "devicePixelRatio", {
      value: originalDpr,
      configurable: true,
    });
    Object.defineProperty(window.navigator, "userAgent", {
      value: originalUa,
      configurable: true,
    });
  });

  it("accepts a drop that lands on a toast floating over the pane", async () => {
    // Round 1. `ToastHost` is `fixed bottom-4 right-4 z-[60]` and 24rem wide,
    // and its error cards stay until dismissed — so a z-order gate asking "is
    // what is painted here part of my pane?" made the bottom-right corner of
    // this pane refuse drops for as long as one error was on screen. jsdom has
    // no `elementFromPoint`, so that branch only ran when a test supplied one;
    // the gate no longer asks, and this pins that nothing painted over a pane
    // can refuse a drop on its own account.
    await renderTab();
    const toastCard = document.createElement("div");
    document.body.appendChild(toastCard);
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      writable: true,
      value: () => toastCard,
    });

    await drop(["/host/a.png"], { x: 700, y: 550 });
    expect(uploadFileToContainer).toHaveBeenCalled();

    delete (document as Partial<Document>).elementFromPoint;
    toastCard.remove();
  });

  it("refuses a drop while a dialog is open, toast painted over it or not", async () => {
    // Round 2, which is the reason this file exists in its current shape. The
    // refusal pushes a toast; `ToastHost` is `z-[60]` and the `Modal` backdrop
    // is `z-50` in the same stacking context, so the *toast* becomes the
    // topmost element over a covered pane. A gate that asked `elementFromPoint`
    // "is a blocker painted here?" then answered no and uploaded into the
    // directory the dialog was covering — one refused drop was all it took to
    // open the hole. Both stubs below therefore have to be refused.
    await renderTab();
    const backdrop = document.createElement("div");
    backdrop.setAttribute("data-blocks-drop", "true");
    document.body.appendChild(backdrop);
    const toastCard = document.createElement("div"); // z-[60], above the backdrop
    document.body.appendChild(toastCard);
    const stub = (top: Element) =>
      Object.defineProperty(document, "elementFromPoint", {
        configurable: true,
        writable: true,
        value: () => top,
      });

    stub(backdrop);
    await drop(["/host/a.png"], { x: 400, y: 300 });
    expect(uploadFileToContainer).not.toHaveBeenCalled();

    stub(toastCard);
    await drop(["/host/a.png"], { x: 700, y: 550 });
    expect(uploadFileToContainer).not.toHaveBeenCalled();

    delete (document as Partial<Document>).elementFromPoint;
    toastCard.remove();
    backdrop.remove();
  });

  it("highlights the pane while a drag hovers it, and drops the highlight on leave", async () => {
    await renderTab();
    await act(async () => {
      await dragHandler?.({
        payload: { type: "over", position: { x: 100, y: 100 }, paths: [] },
      });
    });
    expect(screen.getByText(/Drop files into \/workspace/)).toBeTruthy();
    await act(async () => {
      await dragHandler?.({ payload: { type: "leave" } });
    });
    expect(screen.queryByText(/Drop files into/)).toBeNull();
  });
});

describe("FilesTab save to host", () => {
  it("copies a file out to the path the user picks", async () => {
    await renderTab();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Save to host… — notes.txt" }));
    });
    expect(downloadContainerFile).toHaveBeenCalledWith("p1", "/workspace/notes.txt", "/host/out");
  });

  it("does not offer a directory download, which cannot work", async () => {
    await renderTab();
    expect(screen.queryByRole("button", { name: "Save to host… — src" })).toBeNull();
  });
});

describe("FilesTab drop hit test", () => {
  it("uploads nothing when a dialog is covering the pane", async () => {
    // The pane still has its rect underneath the viewer's `fixed inset-0`
    // portal, which is exactly why a rect alone was the wrong test.
    readContainerFile.mockResolvedValue(contents("hello"));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("notes.txt"));
    });
    await screen.findByRole("dialog");

    await drop(["/host/a.png"]);

    expect(uploadFileToContainer).not.toHaveBeenCalled();
  });

  it("does not paint the hint under a dialog either", async () => {
    readContainerFile.mockResolvedValue(contents("hello"));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("notes.txt"));
    });
    await screen.findByRole("dialog");

    await act(async () => {
      await dragHandler?.({ payload: { type: "over", position: { x: 100, y: 100 }, paths: [] } });
    });
    expect(screen.queryByText(/Drop files into/)).toBeNull();
  });
});

describe("FilesTab grid focus", () => {
  it("gives the grid exactly one tab stop and moves it with the arrows", async () => {
    // Every row used to be `tabIndex={0}`: a 400-entry directory was ~1200 tab
    // stops and Tab could not get out of the list.
    await renderTab();
    expect(gridRows()).toHaveLength(3); // .. , src, notes.txt
    expect(tabStops()).toHaveLength(1);
    expect(tabStops()[0].getAttribute("data-file-row")).toBe("..");

    fireEvent.keyDown(tabStops()[0], { key: "ArrowDown" });
    expect(tabStops()).toHaveLength(1);
    expect(tabStops()[0].getAttribute("data-file-row")).toBe("src");
    expect(document.activeElement).toBe(tabStops()[0]);

    fireEvent.keyDown(tabStops()[0], { key: "End" });
    expect(tabStops()[0].getAttribute("data-file-row")).toBe("notes.txt");
    fireEvent.keyDown(tabStops()[0], { key: "Home" });
    expect(tabStops()[0].getAttribute("data-file-row")).toBe("..");
  });

  it("keeps focus inside the grid after Enter opens a directory", async () => {
    // Rows are keyed by name, so navigating unmounts the focused `<tr>` — and
    // nothing used to re-focus, which ejected the user to `<body>`.
    await renderTab();
    const row = screen.getByText("src").closest("tr")!;
    row.focus();
    listContainerFiles.mockResolvedValueOnce([
      entry("index.ts", { path: "/workspace/src/index.ts" }),
    ]);
    await act(async () => {
      fireEvent.keyDown(row, { key: "Enter" });
    });

    expect(screen.getByText("index.ts")).toBeTruthy();
    expect(document.activeElement).not.toBe(document.body);
    expect((document.activeElement as HTMLElement).closest("tr[data-file-row]")).toBeTruthy();
    expect(tabStops()).toHaveLength(1);
  });

  it("puts focus back on the row after a rename is abandoned", async () => {
    await renderTab();
    const row = screen.getByText("notes.txt").closest("tr")!;
    fireEvent.keyDown(row, { key: "F2" });
    const input = screen.getByLabelText("New name for notes.txt");
    await act(async () => {
      fireEvent.keyDown(input, { key: "Escape" });
    });
    expect(document.activeElement).toBe(
      gridRows().find((r) => r.getAttribute("data-file-row") === "notes.txt"),
    );
  });

  it("follows a committed rename to the row's new name", async () => {
    // Explicit, because `clearAllMocks` clears calls but not implementations,
    // and an earlier test in this file leaves this one rejecting.
    renameContainerPath.mockResolvedValue("/workspace/renamed.txt");
    await renderTab();
    fireEvent.click(screen.getByRole("button", { name: "Rename — notes.txt" }));
    const input = screen.getByLabelText("New name for notes.txt");
    fireEvent.change(input, { target: { value: "renamed.txt" } });
    listContainerFiles.mockResolvedValueOnce([
      entry("src", { is_directory: true, path: "/workspace/src" }),
      entry("renamed.txt"),
    ]);
    await act(async () => {
      fireEvent.blur(input);
    });
    expect(document.activeElement).toBe(
      gridRows().find((r) => r.getAttribute("data-file-row") === "renamed.txt"),
    );
  });
});

describe("FilesTab grid semantics", () => {
  it("names its columns", async () => {
    await renderTab();
    for (const name of ["Name", "Size", "Modified", "Actions"]) {
      expect(screen.getByRole("columnheader", { name })).toBeTruthy();
    }
  });

  it("says folder or file in words, not in hue and a hidden emoji", async () => {
    await renderTab();
    const dir = screen.getByText("src").closest("tr")!;
    const plain = screen.getByText("notes.txt").closest("tr")!;
    expect(dir.textContent).toContain("Folder");
    expect(plain.textContent).toContain("File");
  });

  it("keeps the visible label inside the accessible name (WCAG 2.5.3)", async () => {
    await renderTab();
    const rename = screen.getByRole("button", { name: "Rename — notes.txt" });
    expect(rename.textContent).toBe("Rename");
    expect(rename.getAttribute("aria-label")).toContain("Rename");
    const saveTo = screen.getByRole("button", { name: "Save to host… — notes.txt" });
    expect(saveTo.getAttribute("aria-label")).toContain(saveTo.textContent!);
  });

  it("mounts the live region empty, then fills it", async () => {
    // A `role="status"` node inserted already carrying its text is frequently
    // not announced at all, which is how every one of these went by in silence.
    await renderTab();
    const live = screen.getByRole("status");
    expect(live.textContent).toBe("");

    await drop(["/host/a.png"]);
    // Same node throughout — it is never unmounted.
    expect(screen.getByRole("status")).toBe(live);
    expect(live.textContent).toContain("Uploaded 1 item");
  });

  it("keeps a listing failure inline, where the rows it explains are missing", async () => {
    // The one failure that does *not* go to the toast host: it is on screen,
    // in context, and there is nothing for it to scroll behind.
    listContainerFiles.mockRejectedValue("Permission denied");
    await renderTab();
    expect(screen.getByRole("alert").textContent).toContain("Permission denied");
  });
});

describe("FilesTab overwrite prompt", () => {
  it("asks before replacing, and re-uploads with overwrite on Replace", async () => {
    uploadFileToContainer.mockRejectedValueOnce("FILE_EXISTS: /workspace/notes.txt already exists");
    await renderTab();

    const pending = dropWithoutWaiting(["/host/notes.txt"]);
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain("notes.txt");

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace" }));
      await pending;
    });

    expect(uploadFileToContainer).toHaveBeenLastCalledWith(
      "p1",
      "/host/notes.txt",
      "/workspace",
      true,
    );
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("uploads nothing more on Skip", async () => {
    uploadFileToContainer.mockRejectedValueOnce("FILE_EXISTS: /workspace/notes.txt already exists");
    await renderTab();

    const pending = dropWithoutWaiting(["/host/notes.txt"]);
    await screen.findByRole("dialog");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Skip" }));
      await pending;
    });

    expect(uploadFileToContainer).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("offers the blanket answers only when files are queued behind this one", async () => {
    uploadFileToContainer.mockRejectedValueOnce("FILE_EXISTS: /workspace/a.txt already exists");
    await renderTab();

    const pending = dropWithoutWaiting(["/host/a.txt", "/host/b.txt"]);
    await screen.findByRole("dialog");
    expect(screen.getByRole("button", { name: "Replace all" })).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Skip all" }));
      await pending;
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

/**
 * Dismissal. `Modal` gives every dialog Escape, a ✕ and click-outside for free,
 * and `OverwriteConfirmModal` maps all three onto `onChoose("skip")` — because
 * the destructive answer has to be chosen, and because a dialog that is closed
 * rather than answered must not leave the batch waiting forever or throw away
 * the files behind it.
 */
describe("FilesTab overwrite prompt dismissal", () => {
  /**
   * Drop two files where the first name is taken, and stop at the dialog. The
   * unsettled batch comes back wrapped — returning it bare from an `async`
   * helper would adopt it, and awaiting the helper would then wait for an
   * upload that cannot proceed until the helper has returned.
   */
  async function dropIntoConflict(): Promise<{ batch: Promise<void> | undefined }> {
    uploadFileToContainer.mockRejectedValueOnce("FILE_EXISTS: /workspace/a.txt already exists");
    await renderTab();
    const batch = dropWithoutWaiting(["/host/a.txt", "/host/b.txt"]);
    await screen.findByRole("dialog");
    return { batch };
  }

  /** What every dismissal has to leave behind: one skip, one upload, no clobber. */
  function expectSkippedAndCarriedOn() {
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(uploadFileToContainer).toHaveBeenCalledTimes(2);
    expect(uploadFileToContainer).toHaveBeenLastCalledWith("p1", "/host/b.txt", "/workspace");
    expect(uploadFileToContainer.mock.calls.some((call) => call[3] === true)).toBe(false);
    expect(screen.getByRole("status").textContent).toContain("skipped 1");
  }

  it("counts Escape as a Skip", async () => {
    const { batch } = await dropIntoConflict();
    await act(async () => {
      fireEvent.keyDown(document, { key: "Escape" });
      await batch;
    });
    expectSkippedAndCarriedOn();
  });

  it("counts the ✕ as a Skip", async () => {
    const { batch } = await dropIntoConflict();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Close dialog" }));
      await batch;
    });
    expectSkippedAndCarriedOn();
  });

  it("counts a click on the backdrop as a Skip", async () => {
    const { batch } = await dropIntoConflict();
    // The overlay is the dialog panel's parent — `Modal` only closes when the
    // click landed on the overlay itself, not on anything inside the panel.
    const overlay = screen.getByRole("dialog").parentElement!;
    await act(async () => {
      fireEvent.click(overlay);
      await batch;
    });
    expectSkippedAndCarriedOn();
  });

  it("does not dismiss on a click inside the dialog", async () => {
    const { batch } = await dropIntoConflict();
    fireEvent.click(screen.getByRole("dialog"));
    expect(screen.queryByRole("dialog")).not.toBeNull();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace" }));
      await batch;
    });
    expect(uploadFileToContainer).toHaveBeenNthCalledWith(2, "p1", "/host/a.txt", "/workspace", true);
  });
});
