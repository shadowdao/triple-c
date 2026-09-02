import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useNotes } from "./useNotes";
import { useAppState } from "../store/appState";
import type { Note } from "../lib/types";

const listNotes = vi.fn();
const saveNote = vi.fn();
const deleteNote = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listNotes: (p: string) => listNotes(p),
  saveNote: (p: string, n: Note) => saveNote(p, n),
  deleteNote: (p: string, id: string) => deleteNote(p, id),
}));

const note = (over: Partial<Note> = {}): Note => ({
  id: "n1",
  title: "Deploy",
  body: "one\ntwo",
  pinned: false,
  created_at: "2026-09-01T00:00:00Z",
  updated_at: "2026-09-01T00:00:00Z",
  ...over,
});

/** The toasts the hook pushed. The store is real, so this is what a user sees. */
const toasts = () => useAppState.getState().toasts;

/**
 * A stand-in for the Rust store: one list per project, upsert and delete
 * applied to it, `list_notes` reading it back. Several of these tests are about
 * what the *list* looks like after a sequence of writes, which a per-call
 * `mockResolvedValueOnce` cannot express.
 */
function fakeBackend(initial: Record<string, Note[]> = {}) {
  const files: Record<string, Note[]> = { ...initial };
  listNotes.mockImplementation(async (p: string) => [...(files[p] ?? [])]);
  saveNote.mockImplementation(async (p: string, n: Note) => {
    const list = files[p] ?? (files[p] = []);
    const at = list.findIndex((x) => x.id === n.id);
    if (at === -1) list.unshift(n);
    else list[at] = n;
    return n;
  });
  deleteNote.mockImplementation(async (p: string, id: string) => {
    files[p] = (files[p] ?? []).filter((x) => x.id !== id);
  });
  return files;
}

beforeEach(() => {
  vi.clearAllMocks();
  // The cache is shared app state now, so it has to be reset like any other.
  useAppState.setState({ notesByProject: {}, notesLoading: {}, toasts: [] });
  listNotes.mockResolvedValue([note()]);
  saveNote.mockImplementation(async (_p: string, n: Note) => n);
  deleteNote.mockResolvedValue(undefined);
});

describe("useNotes", () => {
  it("loads a project's notes on mount", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(listNotes).toHaveBeenCalledWith("p1");
    expect(result.current.notes).toHaveLength(1);
  });

  it("reports a failed save instead of swallowing it", async () => {
    // Silent save failure is data loss: the user sees their text on screen and
    // believes it is stored. Same reason `useSaveState` exists.
    saveNote.mockRejectedValueOnce(new Error("disk full"));
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.saveNote(note({ body: "edited" }));
    });

    expect(ok).toBe(false);
    expect(result.current.saveState.status).toBe("failed");
    expect(toasts()).toHaveLength(1);
  });

  it("replaces the saved note in place rather than appending", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    // Mock the re-read to return the edited note
    listNotes.mockResolvedValueOnce([note({ body: "edited" })]);

    await act(async () => {
      await result.current.saveNote(note({ body: "edited" }));
    });

    expect(result.current.notes).toHaveLength(1);
    expect(result.current.notes[0].body).toBe("edited");
  });

  it("drops a deleted note from the list", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.deleteNote("n1");
    });

    expect(deleteNote).toHaveBeenCalledWith("p1", "n1");
    expect(result.current.notes).toHaveLength(0);
  });

  it("does not load anything for an empty project id", async () => {
    // The dock renders with no project selected; it must not fire a command
    // for the empty string.
    renderHook(() => useNotes(""));
    await waitFor(() => expect(listNotes).not.toHaveBeenCalled());
  });

  it("clears the first project's notes when the projectId changes to another non-empty value", async () => {
    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) => useNotes(projectId),
      { initialProps: { projectId: "p1" } },
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.notes).toHaveLength(1);

    // Change to a different project before the new fetch resolves
    listNotes.mockImplementationOnce(() => new Promise(() => {})); // never resolves
    rerender({ projectId: "p2" });

    // The old notes should be cleared immediately
    expect(result.current.notes).toHaveLength(0);
  });

  it("leaves no stale notes on screen when a load fails", async () => {
    listNotes.mockResolvedValueOnce([note()]);
    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) => useNotes(projectId),
      { initialProps: { projectId: "p1" } },
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.notes).toHaveLength(1);

    // Switch to a project whose load fails
    listNotes.mockRejectedValueOnce(new Error("load failed"));
    rerender({ projectId: "p2" });
    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(result.current.notes).toHaveLength(0);
    expect(toasts()).toHaveLength(1);
  });

  it("ends with the list the backend returned when saving a new note", async () => {
    // Initially one note
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.notes).toHaveLength(1);

    // Saving a new note (not in the current list) re-reads and ends with the backend's list
    const newNote = note({ id: "n2", title: "New" });
    listNotes.mockResolvedValueOnce([newNote, note()]);

    await act(async () => {
      await result.current.saveNote(newNote);
    });

    expect(result.current.notes).toHaveLength(2);
    expect(result.current.notes[0].id).toBe("n2");
  });

  it("re-reads the list after a successful save rather than patching in place", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    const callCountBefore = listNotes.mock.calls.length;
    listNotes.mockResolvedValueOnce([note({ body: "edited" })]);

    await act(async () => {
      await result.current.saveNote(note({ body: "edited" }));
    });

    // listNotes should be called again after the save
    expect(listNotes).toHaveBeenCalledTimes(callCountBefore + 1);
  });

  it("does not overwrite the new project's notes when a stale save resolves", async () => {
    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) => useNotes(projectId),
      { initialProps: { projectId: "p1" } },
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.notes[0].id).toBe("n1");

    // Start a save for p1 that hangs
    let resolveSave: ((note: Note) => void) | undefined;
    saveNote.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveSave = resolve;
        }),
    );

    let savePromise: Promise<boolean> | undefined;
    await act(async () => {
      savePromise = result.current.saveNote(note({ id: "n1" }));
    });

    // Switch to p2 while the save is in flight
    listNotes.mockResolvedValueOnce([note({ id: "n2", title: "Project 2 Note" })]);
    rerender({ projectId: "p2" });
    await waitFor(() => expect(result.current.loading).toBe(false));

    // Now p2's note should be displayed
    expect(result.current.notes).toHaveLength(1);
    expect(result.current.notes[0].id).toBe("n2");

    // Resolve the stale p1 save
    listNotes.mockResolvedValueOnce([note({ id: "n1", body: "edited" })]);
    await act(async () => {
      resolveSave?.(note({ id: "n1", body: "edited" }));
      await savePromise;
    });

    // p2's note should still be displayed, not p1's
    expect(result.current.notes).toHaveLength(1);
    expect(result.current.notes[0].id).toBe("n2");
  });

  it("keeps the notes already on screen when a refresh fails", async () => {
    // The second surface mounting for a project is a refresh behind a list the
    // user is already reading. One shared cache means a failed refresh would
    // otherwise blank both panels.
    fakeBackend({ p1: [note()] });
    const tab = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(tab.result.current.loading).toBe(false));

    listNotes.mockRejectedValueOnce(new Error("read failed"));
    const dock = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(toasts()).toHaveLength(1));

    expect(tab.result.current.notes).toHaveLength(1);
    expect(dock.result.current.notes).toHaveLength(1);
  });

  it("does not report the old project's save on the new project's indicator", async () => {
    // The indicator is per-panel and reads "Saved ✓". Firing it after a switch
    // tells the user their *current* project was written when it was not.
    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) => useNotes(projectId),
      { initialProps: { projectId: "p1" } },
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    let resolveSave: ((n: Note) => void) | undefined;
    saveNote.mockImplementationOnce(
      () => new Promise((resolve) => (resolveSave = resolve)),
    );
    let savePromise: Promise<boolean> | undefined;
    await act(async () => {
      savePromise = result.current.saveNote(note({ body: "edited" }));
    });

    rerender({ projectId: "p2" });
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      resolveSave?.(note({ body: "edited" }));
      await savePromise;
    });

    expect(result.current.saveState.status).toBe("idle");
  });

  it("still reports a save on the indicator of the project it was made for", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.saveNote(note({ body: "edited" }));
    });

    expect(result.current.saveState.status).toBe("saved");
  });

  it("serialises a project's writes so an edit cannot be re-inserted after its delete", async () => {
    // Clicking Delete while the textarea has focus fires blur first, so a save
    // and a delete go out back to back. The Rust write lock stops them
    // interleaving but does not order them: a delete that wins the lock is
    // undone by the upsert behind it, and the note comes back on next load.
    const files = fakeBackend({ p1: [note()] });
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    const order: string[] = [];
    saveNote.mockImplementationOnce(async (p: string, n: Note) => {
      await new Promise((r) => setTimeout(r, 20));
      order.push("save");
      files[p] = [n];
      return n;
    });
    deleteNote.mockImplementationOnce(async (p: string, id: string) => {
      order.push("delete");
      files[p] = (files[p] ?? []).filter((x) => x.id !== id);
    });

    await act(async () => {
      const save = result.current.saveNote(note({ body: "typo fixed" }));
      const del = result.current.deleteNote("n1");
      await Promise.all([save, del]);
    });

    expect(order).toEqual(["save", "delete"]);
    expect(files.p1).toHaveLength(0);
    expect(result.current.notes).toHaveLength(0);
  });

  it("keeps a new note when another note is saved right after it", async () => {
    // A purely local draft used to be wiped by the next re-read: two clicks of
    // "New note", type in the second, blur, and the first row was gone.
    const files = fakeBackend({ p1: [note()] });
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    let first: Note | null = null;
    await act(async () => {
      first = await result.current.createNote();
      await result.current.createNote();
    });
    expect(result.current.notes).toHaveLength(3);

    await act(async () => {
      await result.current.saveNote(note({ body: "edited" }));
    });

    expect(result.current.notes).toHaveLength(3);
    expect(result.current.notes.some((n) => n.id === first!.id)).toBe(true);
    expect(files.p1).toHaveLength(3);
  });

  it("shares one cache between every hook watching the same project", async () => {
    // The Project Home sub-tab and the dock both mount a panel for the same
    // project. Two caches meant an edit in one was invisible to the other, and
    // the other's next blur wrote its stale copy back over it.
    fakeBackend({ p1: [note()] });
    const tab = renderHook(() => useNotes("p1"));
    const dock = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(tab.result.current.loading).toBe(false));
    await waitFor(() => expect(dock.result.current.loading).toBe(false));

    // One read for both — the in-flight flag is per project, not per hook.
    expect(listNotes).toHaveBeenCalledTimes(1);

    await act(async () => {
      await dock.result.current.saveNote(note({ body: "written in the dock" }));
    });

    expect(tab.result.current.notes[0].body).toBe("written in the dock");
    expect(tab.result.current.notes).toBe(dock.result.current.notes);
  });

  it("does not blank an already-loaded list when a second panel mounts", async () => {
    fakeBackend({ p1: [note()] });
    const tab = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(tab.result.current.loading).toBe(false));

    const dock = renderHook(() => useNotes("p1"));
    // No "Loading notes…" flash on the second surface.
    expect(dock.result.current.loading).toBe(false);
    expect(dock.result.current.notes).toHaveLength(1);
  });

  it("does not let a slow mount read overwrite a fresher post-save refresh", async () => {
    // One gesture, two requests. The tab is already loaded; the user clicks the
    // dock toggle with the textarea focused, so `blur` → `saveNote` and the
    // dock's mount → `list_notes` are issued in the same tick. The save's
    // re-read writes the post-save list; the mount's read — issued earlier,
    // still in flight — must not then land its pre-save snapshot on top of it.
    const files = fakeBackend({ p1: [note({ body: "before" })] });
    const tab = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(tab.result.current.loading).toBe(false));
    expect(tab.result.current.notes[0].body).toBe("before");

    // The dock's mount read: it snapshots the list as it is *now* (pre-save)
    // and hangs, standing in for a plain read that is slower than
    // `save_note`'s double-fsync write plus the re-read behind it.
    let releaseMountRead: (() => void) | undefined;
    listNotes.mockImplementationOnce(async (p: string) => {
      const preSave = [...(files[p] ?? [])];
      await new Promise<void>((resolve) => {
        releaseMountRead = resolve;
      });
      return preSave;
    });
    const dock = renderHook(() => useNotes("p1"));
    expect(releaseMountRead).toBeDefined();

    // The save and its re-read complete while that read is still out.
    await act(async () => {
      await tab.result.current.saveNote(note({ body: "after" }));
    });
    expect(tab.result.current.notes[0].body).toBe("after");

    // Now the stale read lands.
    await act(async () => {
      releaseMountRead!();
      await Promise.resolve();
    });

    expect(tab.result.current.notes[0].body).toBe("after");
    expect(dock.result.current.notes[0].body).toBe("after");
  });

  it("does not let a slow mount read resurrect a note deleted while it was in flight", async () => {
    // The other half of the same ordering rule: a confirmed delete is newer
    // than any read issued before it finished, so the read's pre-delete list
    // must not be written back over the shortened one.
    const files = fakeBackend({ p1: [note()] });
    const tab = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(tab.result.current.loading).toBe(false));

    let releaseMountRead: (() => void) | undefined;
    listNotes.mockImplementationOnce(async (p: string) => {
      const preDelete = [...(files[p] ?? [])];
      await new Promise<void>((resolve) => {
        releaseMountRead = resolve;
      });
      return preDelete;
    });
    const dock = renderHook(() => useNotes("p1"));
    expect(releaseMountRead).toBeDefined();

    await act(async () => {
      await tab.result.current.deleteNote("n1");
    });
    expect(tab.result.current.notes).toHaveLength(0);

    await act(async () => {
      releaseMountRead!();
      await Promise.resolve();
    });

    expect(tab.result.current.notes).toHaveLength(0);
    expect(dock.result.current.notes).toHaveLength(0);
  });
});
