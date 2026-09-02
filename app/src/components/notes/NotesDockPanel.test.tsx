import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import NotesDockPanel from "./NotesDockPanel";
import type { Note } from "../../lib/types";

const saveNote = vi.fn(async () => true);
const deleteNote = vi.fn(async () => true);
const createNote = vi.fn();
let notes: Note[] = [];
let loading = false;

vi.mock("../../hooks/useNotes", () => ({
  useNotes: () => ({
    notes,
    loading,
    saveState: { status: "idle", error: null },
    createNote,
    saveNote,
    deleteNote,
  }),
}));

const sendProps: Record<string, unknown>[] = [];
vi.mock("./SendToAgentButton", () => ({
  default: (props: Record<string, unknown>) => {
    sendProps.push(props);
    return <button type="button">Send to agent</button>;
  },
}));

const note = (over: Partial<Note> = {}): Note => ({
  id: "n1",
  title: "Deploy steps",
  body: "one\ntwo",
  pinned: false,
  created_at: "2026-09-01T00:00:00Z",
  updated_at: "2026-09-01T00:00:00Z",
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  sendProps.length = 0;
  notes = [];
  loading = false;
});

describe("NotesDockPanel", () => {
  it("says it is loading rather than flashing an empty state", () => {
    loading = true;
    render(<NotesDockPanel projectId="p1" />);
    expect(screen.getByText(/loading notes/i)).toBeInTheDocument();
  });

  it("offers a first note when the project has none", async () => {
    render(<NotesDockPanel projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: /new note/i }));
    await waitFor(() => expect(createNote).toHaveBeenCalled());
  });

  // The point of the redesign: the dock spends its height on the note being
  // written, not on a permanent list of the ones that are not.
  it("shows one note at a time, the rest behind the switcher", () => {
    notes = [note(), note({ id: "n2", title: "Gotchas" })];
    render(<NotesDockPanel projectId="p1" />);

    expect(screen.getByLabelText("Note title")).toHaveValue("Deploy steps");
    expect(screen.queryByText("Gotchas")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));
    expect(screen.getByRole("option", { name: "Gotchas" })).toBeInTheDocument();
  });

  it("switches to the note picked from the list", () => {
    notes = [note(), note({ id: "n2", title: "Gotchas", body: "careful" })];
    render(<NotesDockPanel projectId="p1" />);

    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));
    fireEvent.click(screen.getByRole("option", { name: "Gotchas" }));

    expect(screen.getByLabelText("Note title")).toHaveValue("Gotchas");
    expect(screen.getByLabelText("Note body")).toHaveValue("careful");
  });

  it("saves the body when it loses focus, and not before", () => {
    notes = [note()];
    render(<NotesDockPanel projectId="p1" />);
    const body = screen.getByLabelText("Note body");

    fireEvent.change(body, { target: { value: "one\ntwo\nthree" } });
    expect(saveNote).not.toHaveBeenCalled();

    fireEvent.blur(body);
    expect(saveNote).toHaveBeenCalledWith(
      expect.objectContaining({ id: "n1", body: "one\ntwo\nthree" }),
    );
  });

  it("keeps New and Delete in the overflow menu, out of the writing area", async () => {
    notes = [note()];
    render(<NotesDockPanel projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: /note actions/i }));

    fireEvent.click(screen.getByRole("menuitem", { name: /delete note/i }));
    await waitFor(() => expect(deleteNote).toHaveBeenCalledWith("n1"));
  });

  it("opens the note it just created", async () => {
    notes = [note()];
    createNote.mockResolvedValueOnce(note({ id: "n9", title: "" }));
    const view = render(<NotesDockPanel projectId="p1" />);

    fireEvent.click(screen.getByRole("button", { name: /note actions/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: /new note/i }));
    await waitFor(() => expect(createNote).toHaveBeenCalled());

    notes = [note(), note({ id: "n9", title: "" })];
    view.rerender(<NotesDockPanel projectId="p1" />);
    await waitFor(() =>
      expect(screen.getByLabelText("Note title")).toHaveValue(""),
    );
  });

  // The send bar sits on the dock's bottom edge, inside an `overflow-hidden`
  // panel, so both of these are load-bearing rather than cosmetic.
  it("sends from a full-width bar whose menu opens upward", () => {
    notes = [note()];
    render(<NotesDockPanel projectId="p1" />);

    expect(screen.getByRole("button", { name: /send to agent/i })).toBeInTheDocument();
    expect(sendProps.at(-1)).toMatchObject({ fullWidth: true, dropUp: true });
  });

  it("sends what is on screen, not what was last saved", () => {
    notes = [note()];
    render(<NotesDockPanel projectId="p1" />);
    fireEvent.change(screen.getByLabelText("Note body"), {
      target: { value: "edited but not blurred" },
    });

    expect(sendProps.at(-1)).toMatchObject({ body: "edited but not blurred" });
  });
});
