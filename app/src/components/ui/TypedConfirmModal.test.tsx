import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import TypedConfirmModal from "./TypedConfirmModal";

const onConfirm = vi.fn();
const onCancel = vi.fn();

function renderModal(props: Partial<React.ComponentProps<typeof TypedConfirmModal>> = {}) {
  render(
    <TypedConfirmModal
      title="Delete claude config volume"
      expected="whp"
      confirmLabel="Delete config volume"
      onConfirm={onConfirm}
      onCancel={onCancel}
      {...props}
    >
      <p>Everything goes.</p>
    </TypedConfirmModal>,
  );
  return {
    input: screen.getByLabelText(/Type/),
    confirm: screen.getByRole("button", { name: "Delete config volume" }),
  };
}

beforeEach(() => vi.clearAllMocks());

describe("TypedConfirmModal", () => {
  it("is a real dialog, from the Modal primitive", () => {
    renderModal();
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("aria-modal", "true");
  });

  it("keeps the confirm button shut until the name is typed exactly", () => {
    const { input, confirm } = renderModal();
    expect(confirm).toBeDisabled();

    fireEvent.change(input, { target: { value: "wh" } });
    expect(confirm).toBeDisabled();

    fireEvent.change(input, { target: { value: "whp" } });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    expect(onConfirm).toHaveBeenCalledWith("whp");
  });

  it("is case-sensitive, because Api and api are different projects", () => {
    // This gate is the only thing between a misclick on a sorted table of
    // numbers and a project's transcripts, so a near-miss is a miss.
    const { input, confirm } = renderModal({ expected: "Api" });
    fireEvent.change(input, { target: { value: "api" } });
    expect(confirm).toBeDisabled();
    fireEvent.change(input, { target: { value: "Api" } });
    expect(confirm).toBeEnabled();
  });

  it("forgives surrounding whitespace from a paste", () => {
    const { input, confirm } = renderModal();
    fireEvent.change(input, { target: { value: "  whp  " } });
    expect(confirm).toBeEnabled();
  });

  it("announces the gate's state in words rather than only by the button fill", () => {
    const { input } = renderModal();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Waiting for the exact project name.",
    );
    fireEvent.change(input, { target: { value: "whp" } });
    expect(screen.getByRole("status")).toHaveTextContent("Name matches.");
  });

  it("spells out what is lost, from the caller's copy", () => {
    renderModal();
    expect(screen.getByText("Everything goes.")).toBeInTheDocument();
  });

  it("locks itself while the deletion is running", () => {
    render(
      <TypedConfirmModal
        title="Delete claude config volume"
        expected="whp"
        confirmLabel="Delete config volume"
        onConfirm={onConfirm}
        onCancel={onCancel}
        busy
      >
        <p>Everything goes.</p>
      </TypedConfirmModal>,
    );
    // The confirm button reports the work in a word rather than only going
    // grey, so it is found by its busy label, not its idle one.
    expect(screen.getByLabelText(/Type/)).toBeDisabled();
    expect(screen.getByRole("button", { name: "Working…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
  });

  it("cancels without confirming", () => {
    renderModal();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalled();
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("cannot be satisfied by an empty box when there is no name to type", () => {
    const { confirm } = renderModal({ expected: "" });
    expect(confirm).toBeDisabled();
  });
});
