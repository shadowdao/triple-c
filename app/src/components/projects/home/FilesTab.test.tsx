import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor, within } from "@testing-library/react";
import FilesTab from "./FilesTab";
import type { FileContents, FileEntry, Project } from "../../../lib/types";

const listContainerFiles = vi.fn();
const renameContainerPath = vi.fn(async () => "");
const createContainerDirectory = vi.fn(async () => "");
const readContainerFile = vi.fn();
const uploadFilesToContainer = vi.fn();
const downloadContainerFile = vi.fn();

vi.mock("../../../lib/tauri-commands", () => ({
  listContainerFiles: (p: string, path: string) => listContainerFiles(p, path),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
  readContainerFile: (p: string, path: string, max?: number) => readContainerFile(p, path, max),
  uploadFilesToContainer: (p: string, dir: string) => uploadFilesToContainer(p, dir),
  downloadContainerFile: (p: string, path: string) => downloadContainerFile(p, path),
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

/** Every row that is part of the grid's roving tabindex, in order. */
const gridRows = () => Array.from(document.querySelectorAll("tr[data-file-row]"));
/** The rows that are actually tab stops. There must never be more than one. */
const tabStops = () => gridRows().filter((r) => r.getAttribute("tabindex") === "0");

beforeEach(() => {
  vi.clearAllMocks();
  listContainerFiles.mockResolvedValue([
    entry("src", { is_directory: true, path: "/workspace/src" }),
    entry("notes.txt"),
  ]);
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
    // A refusal has to name the way out, and the way out is now the button on
    // the row rather than the `cat`-it-in-a-terminal workaround that existed
    // because the button did not.
    // Scoped to the modal: every file row also carries a "Save to host…"
    // button now, so an unscoped query matches the grid behind the overlay and
    // would pass with the refusal saying nothing at all.
    expect(
      within(screen.getByRole("dialog")).getByText(/Save to host/),
    ).toBeTruthy();
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

  it("says there is no preview, and where to open the file instead", async () => {
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
    expect(rename.getAttribute("aria-label")).toContain(rename.textContent!);
  });

  it("mounts the live region empty, then fills it", async () => {
    // A `role="status"` node inserted already carrying its text is frequently
    // not announced at all, which is how every one of these went by in silence.
    createContainerDirectory.mockResolvedValue("/workspace/new");
    await renderTab();
    const live = screen.getByRole("status");
    expect(live.textContent).toBe("");

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    });
    const input = screen.getByLabelText("New folder name");
    fireEvent.change(input, { target: { value: "new" } });
    await act(async () => {
      fireEvent.blur(input);
    });
    // Same node throughout — it is never unmounted.
    expect(screen.getByRole("status")).toBe(live);
    expect(live.textContent).toContain('Created "new"');
  });

  it("keeps a listing failure inline, where the rows it explains are missing", async () => {
    // The one failure that does *not* go to the toast host: it is on screen,
    // in context, and there is nothing for it to scroll behind.
    listContainerFiles.mockRejectedValue("Permission denied");
    await renderTab();
    expect(screen.getByRole("alert").textContent).toContain("Permission denied");
  });
});

/**
 * The pane's two host-transfer affordances.
 *
 * They are asserted at the *button* level and not only in the hook, because
 * this is the half that was actually lost: the commands behind them had been
 * deleted, but so had the controls, and a working command nobody can reach is
 * the same regression. Neither button names a host path — Rust opens the
 * dialog — so what a click is required to prove is that the container-side
 * argument reaching the backend is the one the user is looking at.
 */
describe("FilesTab host transfers", () => {
  beforeEach(() => {
    uploadFilesToContainer.mockResolvedValue({ uploaded: [], failures: [] });
    downloadContainerFile.mockResolvedValue(4);
  });

  it("uploads into the directory currently on screen", async () => {
    listContainerFiles.mockResolvedValue([entry("src", { is_directory: true })]);
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(screen.getByText("src"));
    });
    uploadFilesToContainer.mockResolvedValueOnce({
      uploaded: ["/workspace/src/a.txt"],
      failures: [],
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Upload…" }));
    });
    expect(uploadFilesToContainer).toHaveBeenCalledWith("p1", "/workspace/src");
  });

  it("offers Save to host on a file and not on a folder", async () => {
    listContainerFiles.mockResolvedValue([
      entry("notes.txt"),
      entry("src", { is_directory: true }),
    ]);
    await renderTab();
    // The accessible name carries the row, per WCAG 2.5.3 — and it is how a
    // per-row action is told apart from every other row's copy of it.
    expect(
      screen.getByRole("button", { name: "Save to host — notes.txt" }),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Save to host — src" }),
    ).toBeNull();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Save to host — notes.txt" }));
    });
    expect(downloadContainerFile).toHaveBeenCalledWith("p1", "/workspace/notes.txt");
  });

  it("does not open the file viewer when Save to host is double-clicked", async () => {
    // Opening a file is a *double*-click on the row, and a double-click on a
    // button inside that row still bubbles — `onClick`'s `stopPropagation` does
    // nothing about it. So an impatient double-click on Save used to save the
    // file and drop the viewer modal over the pane at the same time, on top of
    // the save dialog the backend had just opened.
    listContainerFiles.mockResolvedValue([entry("notes.txt")]);
    readContainerFile.mockResolvedValue(contents("hello"));
    await renderTab();
    await act(async () => {
      fireEvent.doubleClick(
        screen.getByRole("button", { name: "Save to host — notes.txt" }),
      );
    });
    expect(readContainerFile).not.toHaveBeenCalled();
  });
});
