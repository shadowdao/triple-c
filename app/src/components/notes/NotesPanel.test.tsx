import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import NotesPanel from "./NotesPanel";
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

vi.mock("./SendToAgentButton", () => ({
  default: ({ body }: { body: string }) => (
    <button type="button" data-testid="send">{`send:${body}`}</button>
  ),
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
  notes = [];
  loading = false;
});

describe("NotesPanel", () => {
  it("invites the user to start when there are no notes", () => {
    render(<NotesPanel projectId="p1" />);
    expect(screen.getByText(/no notes yet/i)).toBeInTheDocument();
  });

  it("lists notes by title and selects the first", () => {
    notes = [note(), note({ id: "n2", title: "Gotchas" })];
    render(<NotesPanel projectId="p1" />);
    expect(screen.getByRole("button", { name: /deploy steps/i })).toBeInTheDocument();
    expect(screen.getByLabelText("Note body")).toHaveValue("one\ntwo");
  });

  it("shows an untitled note under a placeholder rather than a blank row", () => {
    notes = [note({ title: "" })];
    render(<NotesPanel projectId="p1" />);
    expect(screen.getByRole("button", { name: /untitled note/i })).toBeInTheDocument();
  });

  it("switches the editor when another note is selected", () => {
    notes = [note(), note({ id: "n2", title: "Gotchas", body: "beware" })];
    render(<NotesPanel projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: /gotchas/i }));
    expect(screen.getByLabelText("Note body")).toHaveValue("beware");
  });

  it("saves on blur, not on every keystroke", async () => {
    notes = [note()];
    render(<NotesPanel projectId="p1" />);
    const body = screen.getByLabelText("Note body");

    fireEvent.change(body, { target: { value: "edited" } });
    expect(saveNote).not.toHaveBeenCalled();

    fireEvent.blur(body);
    await waitFor(() => expect(saveNote).toHaveBeenCalledWith(
      expect.objectContaining({ id: "n1", body: "edited" }),
    ));
  });

  it("does not save on blur when nothing changed", async () => {
    // Clicking through notes to read them must not write the file.
    notes = [note()];
    render(<NotesPanel projectId="p1" />);
    fireEvent.blur(screen.getByLabelText("Note body"));
    await waitFor(() => expect(saveNote).not.toHaveBeenCalled());
  });

  it("hands the live editor text to the send button, not the last saved copy", () => {
    // Sending what is on screen is the whole contract: no transform on the way
    // out except the newline substitution.
    notes = [note()];
    render(<NotesPanel projectId="p1" />);
    fireEvent.change(screen.getByLabelText("Note body"), { target: { value: "fresh" } });
    expect(screen.getByTestId("send")).toHaveTextContent("send:fresh");
  });

  it("deletes the selected note and falls back to another", async () => {
    notes = [note(), note({ id: "n2", title: "Gotchas" })];
    render(<NotesPanel projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: /delete note/i }));
    await waitFor(() => expect(deleteNote).toHaveBeenCalledWith("n1"));
  });
});
