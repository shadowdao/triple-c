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
});
