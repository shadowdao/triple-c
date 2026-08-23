import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useFileManager } from "./useFileManager";
import type { FileEntry } from "../lib/types";

const listContainerFiles = vi.fn();
const downloadContainerFile = vi.fn();
const uploadFileToContainer = vi.fn();
const renameContainerPath = vi.fn();
const createContainerDirectory = vi.fn();
const stageContainerFileForDrag = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listContainerFiles: (p: string, path: string) => listContainerFiles(p, path),
  downloadContainerFile: (p: string, c: string, h: string) => downloadContainerFile(p, c, h),
  uploadFileToContainer: (...args: unknown[]) => uploadFileToContainer(...args),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
  readContainerFile: vi.fn(),
  stageContainerFileForDrag: (p: string, path: string) => stageContainerFileForDrag(p, path),
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

const save = vi.fn();
const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: (opts: unknown) => save(opts),
  open: (opts: unknown) => openDialog(opts),
}));

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

describe("useFileManager uploads", () => {
  it("uploads every dropped path into the current directory, then re-lists once", async () => {
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace/app");
    });
    listContainerFiles.mockClear();

    await act(async () => {
      await result.current.uploadPaths(["/host/a.png", "/host/b.png"]);
    });

    expect(uploadFileToContainer).toHaveBeenNthCalledWith(1, "p1", "/host/a.png", "/workspace/app");
    expect(uploadFileToContainer).toHaveBeenNthCalledWith(2, "p1", "/host/b.png", "/workspace/app");
    // One refresh for the batch, not one per file.
    expect(listContainerFiles).toHaveBeenCalledTimes(1);
  });

  it("reports a failed upload but still lists whatever did land", async () => {
    uploadFileToContainer.mockResolvedValueOnce(undefined);
    uploadFileToContainer.mockRejectedValueOnce("File too large to upload (900 MB; limit 256 MB)");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.uploadPaths(["/host/ok.txt", "/host/huge.bin"]);
    });
    // Inline `error` is reserved for the listing failure the user can see in
    // context; a failed upload goes where it cannot scroll away.
    expect(result.current.error).toBeNull();
    expect(toastText()).toContain("too large");
    expect(listContainerFiles).toHaveBeenCalled();
  });

  it("does nothing when the file picker is cancelled", async () => {
    openDialog.mockResolvedValue(null);
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.uploadFile();
    });
    expect(uploadFileToContainer).not.toHaveBeenCalled();
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

describe("useFileManager save to host", () => {
  it("writes to the path the user picked", async () => {
    save.mockResolvedValue("/host/Downloads/a.txt");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.downloadFile(file("a.txt"));
    });
    expect(downloadContainerFile).toHaveBeenCalledWith(
      "p1",
      "/workspace/a.txt",
      "/host/Downloads/a.txt",
    );
  });

  it("reports a refused download — a directory is no longer written as garbage", async () => {
    save.mockResolvedValue("/host/Downloads/src");
    downloadContainerFile.mockRejectedValue("/workspace/src is a folder — download its files individually");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.downloadFile(file("src", { is_directory: true }));
    });
    expect(toastText()).toContain("is a folder");
  });
});

describe("useFileManager drag-out staging", () => {
  it("copies the file onto the host and hands back the host path", async () => {
    stageContainerFileForDrag.mockResolvedValue("/tmp/triple-c-drag-out/s1/a.txt");
    const { result } = renderHook(() => useFileManager("p1"));

    let staged: { hostPath: string; cached: boolean } | null = null;
    await act(async () => {
      staged = await result.current.stageForDrag(file("a.txt"));
    });

    expect(stageContainerFileForDrag).toHaveBeenCalledWith("p1", "/workspace/a.txt");
    expect(staged).toEqual({ hostPath: "/tmp/triple-c-drag-out/s1/a.txt", cached: false });
    // The note is transient — it must not still be sitting there afterwards.
    expect(result.current.busy).toBeNull();
  });

  it("reuses the copy on a second drag of the same entry", async () => {
    // The whole point of the cache: the copy is the slow half of the gesture,
    // and a retry after a drag the OS missed has to be immediate.
    stageContainerFileForDrag.mockResolvedValue("/tmp/triple-c-drag-out/s1/a.txt");
    const { result } = renderHook(() => useFileManager("p1"));

    let second: { hostPath: string; cached: boolean } | null = null;
    await act(async () => {
      await result.current.stageForDrag(file("a.txt"));
      second = await result.current.stageForDrag(file("a.txt"));
    });

    expect(stageContainerFileForDrag).toHaveBeenCalledTimes(1);
    expect(second).toEqual({ hostPath: "/tmp/triple-c-drag-out/s1/a.txt", cached: true });
  });

  it("re-stages once the entry has changed underneath it", async () => {
    // Keyed on size and mtime, so a file edited in the container is copied
    // again rather than dragged out at its old contents.
    stageContainerFileForDrag.mockResolvedValue("/tmp/triple-c-drag-out/s1/a.txt");
    const { result } = renderHook(() => useFileManager("p1"));

    await act(async () => {
      await result.current.stageForDrag(file("a.txt", { size: 10 }));
      await result.current.stageForDrag(file("a.txt", { size: 4096 }));
    });

    expect(stageContainerFileForDrag).toHaveBeenCalledTimes(2);
  });

  it("surfaces a refused staging instead of returning a path that is not there", async () => {
    stageContainerFileForDrag.mockRejectedValue(
      '900 MB is too large to drag out (limit 256 MB) — use "Save to host…" instead.',
    );
    const { result } = renderHook(() => useFileManager("p1"));

    let staged: { hostPath: string; cached: boolean } | null = null;
    await act(async () => {
      staged = await result.current.stageForDrag(file("huge.bin"));
    });

    expect(staged).toBeNull();
    expect(toastText()).toContain("too large to drag out");
    expect(toastText()).toContain("Save to host");
    expect(result.current.busy).toBeNull();
  });

  it("does not cache a failure, so a retry actually retries", async () => {
    stageContainerFileForDrag.mockRejectedValueOnce("Container not running");
    stageContainerFileForDrag.mockResolvedValueOnce("/tmp/triple-c-drag-out/s1/a.txt");
    const { result } = renderHook(() => useFileManager("p1"));

    let staged: { hostPath: string; cached: boolean } | null = null;
    await act(async () => {
      await result.current.stageForDrag(file("a.txt"));
      staged = await result.current.stageForDrag(file("a.txt"));
    });

    expect(stageContainerFileForDrag).toHaveBeenCalledTimes(2);
    expect(staged).toEqual({ hostPath: "/tmp/triple-c-drag-out/s1/a.txt", cached: false });
  });
});

describe("useFileManager stays where the user is", () => {
  it("does not drag the pane back when the user navigates away mid-upload", async () => {
    // The closure captured `/workspace`; the user is in `/workspace/src` by the
    // time the copy finishes. Re-listing the *captured* path is what used to
    // yank them out of the directory they had walked into.
    let failUpload: (reason: unknown) => void = () => {};
    // `Once`, deliberately: `clearAllMocks` clears calls but not
    // implementations, so a never-settling one would hang every test after it.
    uploadFileToContainer.mockImplementationOnce(
      () => new Promise((_resolve, reject) => { failUpload = reject; }),
    );
    const { result } = renderHook(() => useFileManager("p1"));

    let upload!: Promise<void>;
    await act(async () => {
      upload = result.current.uploadPaths(["/host/big.bin"]);
      await Promise.resolve();
    });

    listContainerFiles.mockResolvedValue([file("index.ts", { path: "/workspace/src/index.ts" })]);
    await act(async () => {
      await result.current.navigate("/workspace/src");
    });
    listContainerFiles.mockClear();

    await act(async () => {
      failUpload("cp: no space left on device");
      await upload;
    });

    expect(result.current.currentPath).toBe("/workspace/src");
    expect(result.current.entries.map((e) => e.name)).toEqual(["index.ts"]);
    // No re-list of the directory the upload targeted…
    expect(listContainerFiles).not.toHaveBeenCalled();
    // …and no failure text painted over the listing that replaced it.
    expect(result.current.error).toBeNull();
    expect(toastText()).toContain("no space left");
  });

  it("re-lists when the user stayed put, which is the ordinary case", async () => {
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.navigate("/workspace");
    });
    listContainerFiles.mockClear();
    await act(async () => {
      await result.current.uploadPaths(["/host/a.png"]);
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
    // The pane never left /workspace, so an upload started now targets it.
    await act(async () => {
      await result.current.uploadPaths(["/host/a.png"]);
    });
    expect(uploadFileToContainer).toHaveBeenCalledWith("p1", "/host/a.png", "/workspace");
  });
});

describe("useFileManager overwrite prompt", () => {
  const alreadyThere = "FILE_EXISTS: /workspace/a.txt already exists";

  it("asks rather than clobbering, and replaces on demand", async () => {
    uploadFileToContainer.mockRejectedValueOnce(alreadyThere);
    uploadFileToContainer.mockResolvedValueOnce(undefined);
    const { result } = renderHook(() => useFileManager("p1"));

    let upload!: Promise<void>;
    await act(async () => {
      upload = result.current.uploadPaths(["/host/a.txt"]);
      await Promise.resolve();
    });
    await waitFor(() => expect(result.current.conflict?.name).toBe("a.txt"));
    expect(result.current.conflict?.directory).toBe("/workspace");
    // One file, so there is nothing for a blanket answer to apply to.
    expect(result.current.conflict?.remaining).toBe(0);

    await act(async () => {
      result.current.resolveConflict("replace");
      await upload;
    });

    expect(uploadFileToContainer).toHaveBeenNthCalledWith(2, "p1", "/host/a.txt", "/workspace", true);
    expect(result.current.conflict).toBeNull();
  });

  it("skips without uploading anything when the user says so", async () => {
    uploadFileToContainer.mockRejectedValueOnce(alreadyThere);
    const { result } = renderHook(() => useFileManager("p1"));

    let upload!: Promise<void>;
    await act(async () => {
      upload = result.current.uploadPaths(["/host/a.txt"]);
      await Promise.resolve();
    });
    await waitFor(() => expect(result.current.conflict).not.toBeNull());
    await act(async () => {
      result.current.resolveConflict("skip");
      await upload;
    });

    expect(uploadFileToContainer).toHaveBeenCalledTimes(1);
    // A skip is a choice, not a failure — nothing to report.
    expect(toastText()).not.toContain("could not be uploaded");
  });

  it("asks once for a batch when the answer is Replace all", async () => {
    uploadFileToContainer.mockRejectedValueOnce(alreadyThere);
    uploadFileToContainer.mockResolvedValueOnce(undefined);
    uploadFileToContainer.mockRejectedValueOnce("FILE_EXISTS: /workspace/b.txt already exists");
    uploadFileToContainer.mockResolvedValueOnce(undefined);
    const { result } = renderHook(() => useFileManager("p1"));

    let upload!: Promise<void>;
    await act(async () => {
      upload = result.current.uploadPaths(["/host/a.txt", "/host/b.txt"]);
      await Promise.resolve();
    });
    await waitFor(() => expect(result.current.conflict?.remaining).toBe(1));
    await act(async () => {
      result.current.resolveConflict("replace-all");
      await upload;
    });

    expect(result.current.conflict).toBeNull();
    expect(uploadFileToContainer).toHaveBeenNthCalledWith(4, "p1", "/host/b.txt", "/workspace", true);
  });

  it("leaves an unrelated failure alone — no prompt offering a button that cannot work", async () => {
    uploadFileToContainer.mockRejectedValueOnce("File too large to upload (900 MB; limit 256 MB)");
    const { result } = renderHook(() => useFileManager("p1"));
    await act(async () => {
      await result.current.uploadPaths(["/host/huge.bin"]);
    });
    expect(result.current.conflict).toBeNull();
    expect(toastText()).toContain("too large");
  });
});

describe("useFileManager staged host paths", () => {
  it("recognises a path it staged, and only that path", async () => {
    stageContainerFileForDrag.mockResolvedValue("/tmp/triple-c-drag-out/s1/a.txt");
    const { result } = renderHook(() => useFileManager("p1"));

    expect(result.current.isStagedHostPath("/tmp/triple-c-drag-out/s1/a.txt")).toBe(false);
    await act(async () => {
      await result.current.stageForDrag(file("a.txt"));
    });
    expect(result.current.isStagedHostPath("/tmp/triple-c-drag-out/s1/a.txt")).toBe(true);
    // Same basename, a real host file the user actually wants uploaded.
    expect(result.current.isStagedHostPath("/home/me/a.txt")).toBe(false);
  });
});
