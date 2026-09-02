import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import AddProjectDialog from "./AddProjectDialog";

const add = vi.fn();

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ add }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
}));

/** A promise whose resolution this test controls, so `loading` can be held open. */
function deferred() {
  let resolve!: (v: unknown) => void;
  const promise = new Promise((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function fillValidForm() {
  fireEvent.change(screen.getByLabelText("Project name"), {
    target: { value: "my-project" },
  });
  fireEvent.change(screen.getByLabelText("Folder 1 host path"), {
    target: { value: "/home/user/my-project" },
  });
}

function submitButton() {
  return screen.getByRole("button", { name: /Add Project|Adding/ });
}

describe("AddProjectDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("adds the project with the name and folder entered", async () => {
    add.mockResolvedValue({ id: "p1" });
    const onClose = vi.fn();
    render(<AddProjectDialog onClose={onClose} />);
    fillValidForm();
    fireEvent.click(submitButton());
    await waitFor(() =>
      expect(add).toHaveBeenCalledWith("my-project", [
        { host_path: "/home/user/my-project", mount_name: "my-project" },
      ]),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("keeps the submit button announced, and explains why, while adding", async () => {
    const { promise, resolve } = deferred();
    add.mockReturnValue(promise);
    render(<AddProjectDialog onClose={vi.fn()} />);
    fillValidForm();
    fireEvent.click(submitButton());

    // Native `disabled` would remove the button from the accessibility tree
    // exactly when it has something to say.
    await waitFor(() =>
      expect(submitButton()).toHaveAttribute("aria-disabled", "true"),
    );
    expect(submitButton()).not.toBeDisabled();
    expect(submitButton()).toHaveAccessibleDescription(/being added/i);

    await act(async () => resolve({ id: "p1" }));
  });

  it("ignores clicks and Enter/Space on the submit button while adding", async () => {
    const { promise, resolve } = deferred();
    add.mockReturnValue(promise);
    render(<AddProjectDialog onClose={vi.fn()} />);
    fillValidForm();
    fireEvent.click(submitButton());
    await waitFor(() =>
      expect(submitButton()).toHaveAttribute("aria-disabled", "true"),
    );

    fireEvent.click(submitButton());
    fireEvent.keyDown(submitButton(), { key: "Enter" });
    fireEvent.keyDown(submitButton(), { key: " " });
    expect(add).toHaveBeenCalledTimes(1);

    await act(async () => resolve({ id: "p1" }));
  });

  it("ignores a form submit raised from elsewhere while adding", async () => {
    const { promise, resolve } = deferred();
    add.mockReturnValue(promise);
    render(<AddProjectDialog onClose={vi.fn()} />);
    fillValidForm();
    fireEvent.click(submitButton());
    await waitFor(() =>
      expect(submitButton()).toHaveAttribute("aria-disabled", "true"),
    );

    // Enter in a text field submits a form regardless of the submit button's
    // state, so the handler has to guard itself too.
    // Modal portals to document.body, so the form is not under `container`.
    const form = document.querySelector("form");
    expect(form).not.toBeNull();
    fireEvent.submit(form!);
    expect(add).toHaveBeenCalledTimes(1);

    await act(async () => resolve({ id: "p1" }));
  });

  it("leaves the submit button plainly available when idle", () => {
    render(<AddProjectDialog onClose={vi.fn()} />);
    expect(submitButton()).not.toHaveAttribute("aria-disabled");
    expect(submitButton()).toHaveAccessibleDescription("");
  });
});
