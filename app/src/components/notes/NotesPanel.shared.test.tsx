import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, within, fireEvent, waitFor } from "@testing-library/react";
import NotesPanel from "./NotesPanel";
import { useAppState } from "../../store/appState";
import type { Note } from "../../lib/types";

/**
 * Two panels, one project — the configuration the app actually runs in.
 *
 * `NotesTab` and `NotesDock` both mount a `NotesPanel`, and the dock follows
 * the active tab's project, so opening the dock over a Project Home tab mounts
 * two panels for the *same* project. Every other notes test mounts exactly
 * one, which is precisely the configuration in which a per-panel cache looks
 * correct: it is only with two that an edit made in one is seen — or lost — by
 * the other. `useNotes` is deliberately **not** mocked here; the cache is what
 * is under test.
 */

const files: Record<string, Note[]> = {};

vi.mock("../../lib/tauri-commands", () => ({
  listNotes: async (p: string) => [...(files[p] ?? [])],
  saveNote: async (p: string, n: Note) => {
    const list = files[p] ?? (files[p] = []);
    const at = list.findIndex((x) => x.id === n.id);
    if (at === -1) list.unshift(n);
    else list[at] = n;
    return n;
  },
  deleteNote: async (p: string, id: string) => {
    files[p] = (files[p] ?? []).filter((x) => x.id !== id);
  },
}));

vi.mock("./SendToAgentButton", () => ({
  default: () => <button type="button">Send to agent</button>,
}));

const note = (over: Partial<Note> = {}): Note => ({
  id: "n1",
  title: "Deploy steps",
  body: "one",
  pinned: false,
  created_at: "2026-09-01T00:00:00Z",
  updated_at: "2026-09-01T00:00:00Z",
  ...over,
});

/** The tab and the dock, mounted together the way `App` mounts them. */
function renderBothSurfaces() {
  render(
    <>
      <div data-testid="tab">
        <NotesPanel projectId="p1" />
      </div>
      <div data-testid="dock">
        <NotesPanel projectId="p1" />
      </div>
    </>,
  );
  return {
    tab: () => within(screen.getByTestId("tab")),
    dock: () => within(screen.getByTestId("dock")),
  };
}

beforeEach(() => {
  for (const key of Object.keys(files)) delete files[key];
  files.p1 = [note()];
  useAppState.setState({ notesByProject: {}, notesLoading: {}, toasts: [] });
});

describe("NotesPanel with the tab and the dock both open", () => {
  it("shows an edit made in one surface in the other", async () => {
    const { tab, dock } = renderBothSurfaces();
    await waitFor(() => expect(tab().getByLabelText("Note body")).toHaveValue("one"));

    const dockTitle = dock().getByLabelText("Note title");
    fireEvent.change(dockTitle, { target: { value: "Deploy steps v2" } });
    fireEvent.blur(dockTitle);

    // The other surface's list *and* its editor, not just one of them.
    await waitFor(() =>
      expect(tab().getByRole("button", { name: /deploy steps v2/i })).toBeInTheDocument(),
    );
    expect(tab().getByLabelText("Note title")).toHaveValue("Deploy steps v2");
  });

  it("does not write one surface's stale copy over the other's edit", async () => {
    // The reported repro: edit in the dock, then go back to the tab and edit
    // there. With a cache per panel, the tab committed `{...staleNote, ...}`
    // and the dock's edit was gone from disk with no error and no indicator.
    const { tab, dock } = renderBothSurfaces();
    await waitFor(() => expect(tab().getByLabelText("Note body")).toHaveValue("one"));

    const dockTitle = dock().getByLabelText("Note title");
    fireEvent.change(dockTitle, { target: { value: "Deploy steps v2" } });
    fireEvent.blur(dockTitle);
    await waitFor(() => expect(files.p1[0].title).toBe("Deploy steps v2"));

    const tabBody = tab().getByLabelText("Note body");
    fireEvent.change(tabBody, { target: { value: "two" } });
    fireEvent.blur(tabBody);

    await waitFor(() => expect(files.p1[0].body).toBe("two"));
    expect(files.p1).toHaveLength(1);
    expect(files.p1[0].title).toBe("Deploy steps v2");
  });

  it("reads the project once for both surfaces", async () => {
    // Two panels are two `useNotes`, but the in-flight flag is per project, so
    // mounting the dock over an open Notes tab does not re-read the file.
    const listNotes = vi.spyOn(
      await import("../../lib/tauri-commands"),
      "listNotes",
    );
    renderBothSurfaces();
    await waitFor(() =>
      expect(screen.getAllByLabelText("Note body")[0]).toHaveValue("one"),
    );
    expect(listNotes).toHaveBeenCalledTimes(1);
    listNotes.mockRestore();
  });

  it("keeps text the user is part-way through typing when the other surface saves", async () => {
    // Showing a remote edit must never mean discarding an unsaved local one.
    const { tab, dock } = renderBothSurfaces();
    await waitFor(() => expect(tab().getByLabelText("Note body")).toHaveValue("one"));

    const tabBody = tab().getByLabelText("Note body");
    fireEvent.change(tabBody, { target: { value: "half-typed" } });

    const dockBody = dock().getByLabelText("Note body");
    fireEvent.change(dockBody, { target: { value: "saved in the dock" } });
    fireEvent.blur(dockBody);
    await waitFor(() => expect(files.p1[0].body).toBe("saved in the dock"));

    expect(tabBody).toHaveValue("half-typed");
  });

  it("falls back to another note when the selected one is deleted", async () => {
    // The claim a differently-named test in NotesPanel.test.tsx used to make
    // and could not keep: `useNotes` is mocked there and its list never
    // changes, so the fallback was invisible. Here the list is real.
    files.p1 = [note(), note({ id: "n2", title: "Gotchas", body: "beware" })];
    const { tab } = renderBothSurfaces();
    await waitFor(() => expect(tab().getByLabelText("Note body")).toHaveValue("one"));

    fireEvent.click(tab().getByRole("button", { name: /delete note/i }));

    await waitFor(() => expect(tab().getByLabelText("Note body")).toHaveValue("beware"));
    expect(tab().queryByRole("button", { name: /deploy steps/i })).not.toBeInTheDocument();
    expect(files.p1).toHaveLength(1);
  });

  it("shows a note created in one surface in the other", async () => {
    const { tab, dock } = renderBothSurfaces();
    await waitFor(() => expect(tab().getByLabelText("Note body")).toHaveValue("one"));

    fireEvent.click(dock().getByRole("button", { name: /new note/i }));

    await waitFor(() =>
      expect(tab().getAllByRole("button", { name: /untitled note/i })).toHaveLength(1),
    );
    expect(files.p1).toHaveLength(2);
  });
});
