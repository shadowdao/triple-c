import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useFileManager } from "./useFileManager";
import type { FileEntry } from "../lib/types";

const listContainerFiles = vi.fn();
const renameContainerPath = vi.fn();
const createContainerDirectory = vi.fn();
const uploadFilesToContainer = vi.fn();
const downloadContainerFile = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listContainerFiles: (p: string, path: string) => listContainerFiles(p, path),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
  uploadFilesToContainer: (p: string, dir: string) => uploadFilesToContainer(p, dir),
  downloadContainerFile: (p: string, path: string) => downloadContainerFile(p, path),
  readContainerFile: vi.fn(),
}));

/**
 * Transient failures go to `ToastHost` rather than an inline string — see the
 * comment at the top of `useFileManager`. The store is mocked down to the one
 * method the hook reaches for.
 */
const pushToast = vi.fn();
vi.mock("../store/appState", () => ({
  useAppState: { getState: () => ({ pushToast }) },
}));

/** Everything the hook has said through the toast host, message and detail. */
const toastText = () =>
  pushToast.mock.calls
    .map(([toast]) => `${toast.kind}: ${toast.message} ${toast.detail ?? ""}`)
    .join("\n");

const file = (name: string, extra: Partial<FileEntry> = {}): FileEntry => ({
  name,
  path: `/workspace/${name}`,
  is_directory: false,
  is_symlink: false,
  size: 10,
  modified: "2024-01-01 00:00:00",
  permissions: "644",
  ...extra,
});

beforeEach(() => {
  vi.clearAllMocks();
  listContainerFiles.mockResolvedValue([file("a.txt")]);
  uploadFilesToContainer.mockResolvedValue({ uploaded: [], failures: [] });
  downloadContainerFile.mockResolvedValue(0);
});

describe("useFileManager navigation", () => {
  it("lists a directory and remembers where it is", async () => {
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app");
    });
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace/app");
    expect(result.current.currentPath).toBe("/workspace/app");
    expect(result.current.entries).toHaveLength(1);
  });

  it("surfaces a listing failure rather than showing a stale directory", async () => {
    listContainerFiles.mockRejectedValueOnce("Permission denied");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/root");
    });
    expect(result.current.error).toContain("Permission denied");
    expect(result.current.currentPath).toBe("/workspace");
  });

  it("goes up one level, and stops at the root", async () => {
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app/src");
    });
    await act(async () => {
      result.current.goUp();
    });
    await waitFor(() => expect(result.current.currentPath).toBe("/workspace/app"));

    await act(async () => {
      await result.current.navigate("/");
    });
    listContainerFiles.mockClear();
    await act(async () => {
      result.current.goUp();
    });
    expect(listContainerFiles).not.toHaveBeenCalled();
  });
});

describe("useFileManager rename and mkdir", () => {
  it("sends the bare new name, never a path, and re-lists on success", async () => {
    renameContainerPath.mockResolvedValue("/workspace/renamed.txt");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace");
    });
    listContainerFiles.mockClear();

    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.renameEntry(file("a.txt"), "  renamed.txt  ");
    });
    expect(ok).toBe(true);
    expect(renameContainerPath).toHaveBeenCalledWith("p1", "/workspace/a.txt", "renamed.txt");
    expect(listContainerFiles).toHaveBeenCalledTimes(1);
  });

  it("keeps the editor open and shows what the container said when a rename fails", async () => {
    // Renames outside /workspace legitimately fail; the user needs mv's words.
    renameContainerPath.mockRejectedValue("mv: cannot move '/etc/hosts': Permission denied");
    const { result } = renderHook(() => useFileManager("p1"));
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.renameEntry(file("hosts"), "hosts.bak");
    });
    expect(ok).toBe(false);
    expect(toastText()).toContain("Permission denied");
  });

  it("treats an unchanged name as a no-op rather than a round trip", async () => {
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.renameEntry(file("a.txt"), "a.txt");
    });
    expect(renameContainerPath).not.toHaveBeenCalled();
  });

  it("creates a folder under the current directory", async () => {
    createContainerDirectory.mockResolvedValue("/workspace/app/new");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app");
    });
    await act(async () => {
      await result.current.createFolder(" new ");
    });
    expect(createContainerDirectory).toHaveBeenCalledWith("p1", "/workspace/app", "new");
  });

  it("surfaces a clash instead of silently doing nothing", async () => {
    createContainerDirectory.mockRejectedValue("mkdir: cannot create directory 'src': File exists");
    const { result } = renderHook(() => useFileManager("p1"));
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.createFolder("src");
    });
    expect(ok).toBe(false);
    expect(toastText()).toContain("File exists");
  });
});

describe("useFileManager stays where the user is", () => {
  it("does not drag the pane back when the user navigates away mid-operation", async () => {
    // The closure captured `/workspace`; the user is in `/workspace/src` by the
    // time the rename finishes. Re-listing the *captured* path is what used to
    // yank them out of the directory they had walked into.
    let failRename: (reason: unknown) => void = () => {};
    // `Once`, deliberately: `clearAllMocks` clears calls but not
    // implementations, so a never-settling one would hang every test after it.
    renameContainerPath.mockImplementationOnce(
      () => new Promise((_resolve, reject) => { failRename = reject; }),
    );
    const { result } = renderHook(() => useFileManager("p1"));

    let rename!: Promise<boolean>;
    await act(async () => {
      rename = result.current.renameEntry(file("big.bin"), "bigger.bin");
      await Promise.resolve();
    });

    listContainerFiles.mockResolvedValue([file("index.ts", { path: "/workspace/src/index.ts" })]);
    await act(async () => {
      await result.current.navigate("/workspace/src");
    });
    listContainerFiles.mockClear();

    await act(async () => {
      failRename("mv: no space left on device");
      await rename;
    });

    expect(result.current.currentPath).toBe("/workspace/src");
    expect(result.current.entries.map((e) => e.name)).toEqual(["index.ts"]);
    // No re-list of the directory the rename targeted…
    expect(listContainerFiles).not.toHaveBeenCalled();
    // …and no failure text painted over the listing that replaced it.
    expect(result.current.error).toBeNull();
    expect(toastText()).toContain("no space left");
  });

  it("re-lists when the user stayed put, which is the ordinary case", async () => {
    renameContainerPath.mockResolvedValue("/workspace/b.txt");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace");
    });
    listContainerFiles.mockClear();
    await act(async () => {
      await result.current.renameEntry(file("a.txt"), "b.txt");
    });
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace");
  });

  it("lets the newest listing win when a slow one lands last", async () => {
    // Two listings in flight, and the slower one is not necessarily the older
    // one. Landing last used to set both the rows and the breadcrumb back.
    let landSlow: (entries: FileEntry[]) => void = () => {};
    listContainerFiles.mockImplementationOnce(
      () => new Promise((resolve) => { landSlow = resolve; }),
    );
    listContainerFiles.mockResolvedValueOnce([file("new.txt")]);
    const { result } = renderHook(() => useFileManager("p1"));

    let slow!: Promise<void>;
    await act(async () => {
      slow = result.current.navigate("/workspace/slow");
      await Promise.resolve();
    });
    await act(async () => {
      await result.current.navigate("/workspace/fast");
    });
    expect(result.current.currentPath).toBe("/workspace/fast");

    await act(async () => {
      landSlow([file("stale.txt")]);
      await slow;
    });

    expect(result.current.currentPath).toBe("/workspace/fast");
    expect(result.current.entries.map((e) => e.name)).toEqual(["new.txt"]);
  });

  it("keeps a failed navigation from claiming the directory it never reached", async () => {
    listContainerFiles.mockRejectedValueOnce("Permission denied");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/root");
    });
    listContainerFiles.mockClear();
    // The pane never left /workspace, so a new folder made now lands there.
    await act(async () => {
      await result.current.createFolder("new");
    });
    expect(createContainerDirectory).toHaveBeenCalledWith("p1", "/workspace", "new");
  });
});

/**
 * Refusals the backend already phrased for a person. `ToastHost` renders a
 * `detail` as collapsed monospace behind a "Details" button, so a sentence
 * reported that way is a sentence nobody reads.
 */
describe("useFileManager surfaces written refusals as prose", () => {
  const outsideRoots =
    "Folder path is outside the folders this panel can change (/workspace, /home/claude, /tmp): /etc";

  /** The toast this operation pushed. */
  const lastToast = () => pushToast.mock.calls.at(-1)?.[0];

  it("puts the write-root refusal in the headline, not behind Details", async () => {
    createContainerDirectory.mockRejectedValueOnce(outsideRoots);
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.createFolder("new");
    });

    expect(lastToast().message).toBe(outsideRoots);
    expect(lastToast().detail).toBeUndefined();
    expect(lastToast().message).not.toMatch(/^Error:/);
  });

  it("unwraps an `Error` rather than stamping \"Error:\" on prose", async () => {
    renameContainerPath.mockRejectedValueOnce(new Error(outsideRoots));
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.renameEntry(file("a.txt"), "b.txt");
    });

    expect(lastToast().message).toBe(outsideRoots);
  });

  it("keeps the hook's own headline when the failure is not a written refusal", async () => {
    createContainerDirectory.mockRejectedValueOnce("no space left on device");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.createFolder("new");
    });

    expect(lastToast().message).toBe('Could not create "new"');
    expect(lastToast().detail).toBe("no space left on device");
  });
});

/**
 * Both of these actions are *dialog-driven from Rust* — the hook passes a
 * project and a directory and gets back an answer, and there is deliberately no
 * host path anywhere in this file. What is worth pinning is the vocabulary of
 * that answer, because two of its values look like failure and are not: `null`
 * means the user dismissed the picker, and `0` bytes means an empty file was
 * saved successfully.
 */
describe("useFileManager saving to the host", () => {
  it("treats a zero-byte save as a success", async () => {
    // The bug this exists for: `if (!bytes) return` reads a genuine
    // zero-length file — an empty `.gitkeep`, a truncated log — as a
    // dismissal, so the file lands on the host and the app says nothing at
    // all. The sentinel is `null`, and only `null`.
    downloadContainerFile.mockResolvedValueOnce(0);
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.saveToHost(file("empty.txt"));
    });
    expect(result.current.completed).toContain("empty.txt");
    expect(pushToast).not.toHaveBeenCalled();
  });

  it("says nothing at all when the dialog is dismissed", async () => {
    downloadContainerFile.mockResolvedValueOnce(null);
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.saveToHost(file("a.txt"));
    });
    expect(result.current.completed).toBeNull();
    expect(pushToast).not.toHaveBeenCalled();
  });

  it("names the file in a refusal", async () => {
    downloadContainerFile.mockRejectedValueOnce("/etc/shadow is not readable");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.saveToHost(file("secret.txt"));
    });
    expect(toastText()).toContain("secret.txt");
  });
});

describe("useFileManager uploading from the host", () => {
  it("uploads into the directory on screen and shows the result", async () => {
    uploadFilesToContainer.mockResolvedValueOnce({
      uploaded: ["/workspace/app/one.txt", "/workspace/app/two.txt"],
      failures: [],
    });
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app");
    });
    listContainerFiles.mockClear();
    await act(async () => {
      await result.current.uploadFiles();
    });
    expect(uploadFilesToContainer).toHaveBeenCalledWith("p1", "/workspace/app");
    expect(result.current.completed).toContain("2 files");
    // The directory is named. `target` is captured at click time and the
    // picker is a modal dialog, so "Uploaded 2 files." on its own can be shown
    // in front of a grid those files are not in.
    expect(result.current.completed).toContain("/workspace/app");
    // The new files are only on screen if the listing was asked for again.
    expect(listContainerFiles).toHaveBeenCalledWith("p1", "/workspace/app");
  });

  it("reports every file that failed, not just a count", async () => {
    // "3 of 5 uploaded" without naming the two is not a report — the user
    // cannot tell which ones to retry, or why.
    uploadFilesToContainer.mockResolvedValueOnce({
      uploaded: ["/workspace/ok.txt"],
      failures: [
        "/home/j/Pictures is a folder — upload its files individually.",
        "/home/j/vm.img is too large to upload (900 MB; limit 256 MB).",
      ],
    });
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.uploadFiles();
    });
    expect(pushToast).toHaveBeenCalledTimes(2);
    expect(toastText()).toContain("is a folder");
    expect(toastText()).toContain("too large");
    // A partial batch still succeeded partially, and the pane must show it.
    expect(result.current.completed).toContain("1 file");
  });

  it("does not refresh when the picker was dismissed", async () => {
    uploadFilesToContainer.mockResolvedValueOnce(null);
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app");
    });
    listContainerFiles.mockClear();
    await act(async () => {
      await result.current.uploadFiles();
    });
    expect(listContainerFiles).not.toHaveBeenCalled();
    expect(pushToast).not.toHaveBeenCalled();
    expect(result.current.completed).toBeNull();
  });

  it("reports a refusal that happened before the picker once, not per file", async () => {
    // No container, not running, or a directory this pane may not write to.
    // There is no selection yet, so there is nothing to enumerate.
    uploadFilesToContainer.mockRejectedValueOnce(
      "Start the project before uploading files — it runs inside the running container.",
    );
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.uploadFiles();
    });
    expect(pushToast).toHaveBeenCalledTimes(1);
    expect(toastText()).toContain("Start the project");
  });

  it("does not drag the pane back when the user navigated during the upload", async () => {
    // The same rule rename and new-folder follow: a slow operation must not
    // relist a directory the user has already left.
    let release: (v: unknown) => void = () => {};
    uploadFilesToContainer.mockReturnValueOnce(
      new Promise((resolve) => {
        release = resolve;
      }),
    );
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app");
    });
    let uploading: Promise<void>;
    act(() => {
      uploading = result.current.uploadFiles();
    });
    await act(async () => {
      await result.current.navigate("/workspace/other");
    });
    listContainerFiles.mockClear();
    await act(async () => {
      release({ uploaded: ["/workspace/app/one.txt"], failures: [] });
      await uploading;
    });
    expect(listContainerFiles).not.toHaveBeenCalled();
    expect(result.current.currentPath).toBe("/workspace/other");
  });
});

describe("useFileManager transfer state", () => {
  it("marks an upload in flight for as long as it runs", async () => {
    // Without this the button stays live: a second click opens a second OS
    // dialog and runs a second concurrent exec, and a slow transfer looks
    // exactly like a click that did nothing.
    let release: (v: unknown) => void = () => {};
    uploadFilesToContainer.mockReturnValueOnce(
      new Promise((resolve) => {
        release = resolve;
      }),
    );
    const { result } = renderHook(() => useFileManager("p1"));
    expect(result.current.uploading).toBe(false);
    let uploading: Promise<void>;
    act(() => {
      uploading = result.current.uploadFiles();
    });
    expect(result.current.uploading).toBe(true);
    await act(async () => {
      release({ uploaded: [], failures: [] });
      await uploading;
    });
    expect(result.current.uploading).toBe(false);
  });

  it("stays in flight through the refresh, not just the transfer", async () => {
    // Clearing the flag the moment the command settled put the button back
    // while the re-listing was still running, so a second click landed
    // mid-refresh on a grid that was still showing the old contents.
    uploadFilesToContainer.mockResolvedValueOnce({
      uploaded: ["/workspace/a.txt"],
      failures: [],
    });
    let finishListing: (v: unknown) => void = () => {};
    listContainerFiles.mockReturnValueOnce(
      new Promise((resolve) => {
        finishListing = resolve;
      }),
    );
    const { result } = renderHook(() => useFileManager("p1"));
    let uploading: Promise<void>;
    act(() => {
      uploading = result.current.uploadFiles();
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    // The transfer is done; the listing it triggered is not.
    expect(result.current.uploading).toBe(true);
    await act(async () => {
      finishListing([file("a.txt")]);
      await uploading;
    });
    expect(result.current.uploading).toBe(false);
  });

  it("clears the upload flag when the transfer fails", async () => {
    // The `catch` returns early, so without a `finally` the button is disabled
    // for the rest of the session — the failure mode is a pane that can never
    // upload again, with no error left on screen to explain it.
    uploadFilesToContainer.mockRejectedValueOnce("Start the project first");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.uploadFiles();
    });
    expect(result.current.uploading).toBe(false);
  });

  it("tracks each save separately, so one finishing does not free another", async () => {
    // The bug this exists for: `savingPath` was a single string. Starting a
    // second save overwrote it, so the first row went live again mid-transfer,
    // and whichever save settled first cleared the flag for both — dismissing
    // the second dialog was enough. A set is what the design needs, because
    // "only the row being saved is disabled" is exactly what makes a second
    // save startable.
    let releaseBig: (v: unknown) => void = () => {};
    let releaseSmall: (v: unknown) => void = () => {};
    downloadContainerFile
      .mockReturnValueOnce(new Promise((r) => { releaseBig = r; }))
      .mockReturnValueOnce(new Promise((r) => { releaseSmall = r; }));

    const { result } = renderHook(() => useFileManager("p1"));
    let big: Promise<void>;
    let small: Promise<void>;
    act(() => { big = result.current.saveToHost(file("big.bin")); });
    expect(result.current.savingPaths.has("/workspace/big.bin")).toBe(true);

    act(() => { small = result.current.saveToHost(file("notes.txt")); });
    // Both, at once — a scalar could only hold the second.
    expect(result.current.savingPaths.has("/workspace/big.bin")).toBe(true);
    expect(result.current.savingPaths.has("/workspace/notes.txt")).toBe(true);

    // The second one finishing must not re-enable the first, which is still
    // streaming. `null` is the dismissal path, which is how this was cheapest
    // to trigger in practice.
    await act(async () => { releaseSmall(null); await small; });
    expect(result.current.savingPaths.has("/workspace/notes.txt")).toBe(false);
    expect(result.current.savingPaths.has("/workspace/big.bin")).toBe(true);

    await act(async () => { releaseBig(10); await big; });
    expect(result.current.savingPaths.size).toBe(0);
  });

  it("clears a row's saving flag when its save fails", async () => {
    downloadContainerFile.mockRejectedValueOnce("Permission denied");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.saveToHost(file("b.txt"));
    });
    expect(result.current.savingPaths.size).toBe(0);
  });
});
