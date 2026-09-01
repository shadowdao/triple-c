import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useNotes } from "./useNotes";
import type { Note } from "../lib/types";

const listNotes = vi.fn();
const saveNote = vi.fn();
const deleteNote = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listNotes: (p: string) => listNotes(p),
  saveNote: (p: string, n: Note) => saveNote(p, n),
  deleteNote: (p: string, id: string) => deleteNote(p, id),
}));

const pushToast = vi.fn();
vi.mock("../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) => selector({ pushToast }),
    { getState: () => ({ pushToast }) },
  ),
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

beforeEach(() => {
  vi.clearAllMocks();
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
    expect(pushToast).toHaveBeenCalled();
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
    expect(pushToast).toHaveBeenCalled();
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
});
