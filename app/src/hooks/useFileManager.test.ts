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
  uploadFileToContainer: (p: string, h: string, d: string) => uploadFileToContainer(p, h, d),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
  readContainerFile: vi.fn(),
  stageContainerFileForDrag: (p: string, path: string) => stageContainerFileForDrag(p, path),
}));

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
    expect(result.current.error).toContain("too large");
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
    expect(result.current.error).toContain("Permission denied");
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
    expect(result.current.error).toContain("File exists");
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
    expect(result.current.error).toContain("is a folder");
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
    expect(result.current.error).toContain("too large to drag out");
    expect(result.current.error).toContain("Save to host");
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
