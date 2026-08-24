import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useFileManager } from "./useFileManager";
import type { FileEntry } from "../lib/types";

const listContainerFiles = vi.fn();
const renameContainerPath = vi.fn();
const createContainerDirectory = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listContainerFiles: (p: string, path: string) => listContainerFiles(p, path),
  renameContainerPath: (p: string, f: string, t: string) => renameContainerPath(p, f, t),
  createContainerDirectory: (p: string, parent: string, n: string) =>
    createContainerDirectory(p, parent, n),
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
