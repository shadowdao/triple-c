import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import NoteSwitcher from "./NoteSwitcher";
import type { Note } from "../../lib/types";

const onTitleChange = vi.fn();
const onCommit = vi.fn();
const onSelect = vi.fn();

const note = (over: Partial<Note> = {}): Note => ({
  id: "n1",
  title: "Deploy steps",
  body: "",
  pinned: false,
  created_at: "2026-09-01T00:00:00Z",
  updated_at: "2026-09-01T00:00:00Z",
  ...over,
});

const setup = (notes: Note[], selectedId = notes[0]?.id ?? "", title = notes[0]?.title ?? "") =>
  render(
    <NoteSwitcher
      notes={notes}
      selectedId={selectedId}
      title={title}
      onTitleChange={onTitleChange}
      onCommit={onCommit}
      onSelect={onSelect}
    />,
  );

beforeEach(() => vi.clearAllMocks());

describe("NoteSwitcher", () => {
  it("edits the title in place, committing on blur", () => {
    setup([note()]);
    const field = screen.getByLabelText("Note title");
    expect(field).toHaveValue("Deploy steps");

    fireEvent.change(field, { target: { value: "Deploy steps v2" } });
    expect(onTitleChange).toHaveBeenCalledWith("Deploy steps v2");
    expect(onCommit).not.toHaveBeenCalled();

    fireEvent.blur(field);
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("keeps the other notes out of the way until asked for", () => {
    setup([note(), note({ id: "n2", title: "Gotchas" })]);
    expect(screen.queryByText("Gotchas")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));
    expect(screen.getByRole("option", { name: "Gotchas" })).toBeInTheDocument();
  });

  it("reports whether the list is open", () => {
    setup([note()]);
    const trigger = screen.getByRole("button", { name: /switch note/i });
    expect(trigger).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("marks the current note as the selected option", () => {
    setup([note(), note({ id: "n2", title: "Gotchas" })], "n2", "Gotchas");
    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));

    expect(screen.getByRole("option", { name: "Gotchas" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("option", { name: "Deploy steps" })).toHaveAttribute(
      "aria-selected",
      "false",
    );
  });

  it("selects a note and closes", () => {
    setup([note(), note({ id: "n2", title: "Gotchas" })]);
    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));
    fireEvent.click(screen.getByRole("option", { name: "Gotchas" }));

    expect(onSelect).toHaveBeenCalledWith("n2");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("names an untitled note rather than showing an empty row", () => {
    setup([note({ title: "   " })]);
    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));
    expect(screen.getByRole("option", { name: "Untitled note" })).toBeInTheDocument();
  });

  // Notes are addressed by id, never by title. Two untitled notes are the
  // ordinary case, and a title-keyed list would collapse them into one row.
  it("lists two notes that share a title as two options", () => {
    setup([note({ id: "n1", title: "" }), note({ id: "n2", title: "" })]);
    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));

    const options = screen.getAllByRole("option", { name: "Untitled note" });
    expect(options).toHaveLength(2);

    fireEvent.click(options[1]);
    expect(onSelect).toHaveBeenCalledWith("n2");
  });

  it("closes on Escape without selecting anything", () => {
    setup([note(), note({ id: "n2", title: "Gotchas" })]);
    fireEvent.click(screen.getByRole("button", { name: /switch note/i }));
    fireEvent.keyDown(document, { key: "Escape" });

    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(onSelect).not.toHaveBeenCalled();
  });
});
