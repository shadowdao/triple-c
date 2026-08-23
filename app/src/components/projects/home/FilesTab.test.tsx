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
  uploadFileToContainer: (p: string, h: string, d: string) => uploadFileToContainer(p, h, d),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
  readContainerFile: (p: string, path: string, max?: number) => readContainerFile(p, path, max),
}));

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
    expect(row.getAttribute("tabindex")).toBe("0");
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
    fireEvent.click(screen.getByRole("button", { name: "Rename notes.txt" }));
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
    fireEvent.click(screen.getByRole("button", { name: "Rename notes.txt" }));
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

  it("shows what the container said when a rename is refused", async () => {
    renameContainerPath.mockRejectedValue("mv: cannot move '/etc/hosts': Permission denied");
    await renderTab();
    fireEvent.click(screen.getByRole("button", { name: "Rename notes.txt" }));
    const input = screen.getByLabelText("New name for notes.txt");
    fireEvent.change(input, { target: { value: "x" } });
    await act(async () => {
      fireEvent.blur(input);
    });
    expect(screen.getByRole("alert").textContent).toContain("Permission denied");
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

  it("divides the payload position by devicePixelRatio", async () => {
    // The native payload is in physical pixels; the rect is in CSS pixels.
    // At dpr 2 a physical (900, 900) is a CSS (450, 450) — inside an 800x600 pane.
    const original = window.devicePixelRatio;
    Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
    await renderTab();
    await drop(["/host/a.png"], { x: 900, y: 900 });
    expect(uploadFileToContainer).toHaveBeenCalled();
    Object.defineProperty(window, "devicePixelRatio", { value: original, configurable: true });
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
      fireEvent.click(screen.getByRole("button", { name: "Save notes.txt to host" }));
    });
    expect(downloadContainerFile).toHaveBeenCalledWith("p1", "/workspace/notes.txt", "/host/out");
  });

  it("does not offer a directory download, which cannot work", async () => {
    await renderTab();
    expect(screen.queryByRole("button", { name: "Save src to host" })).toBeNull();
  });
});
