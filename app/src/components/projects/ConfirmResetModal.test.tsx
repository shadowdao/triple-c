import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import ConfirmResetModal from "./ConfirmResetModal";

/** Modal focuses via rAF so the panel is laid out first; jsdom needs a flush. */
async function flushFocus() {
  await act(async () => {
    vi.advanceTimersByTime(20);
  });
}

describe("ConfirmResetModal", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "setTimeout"] });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  async function renderModal() {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmResetModal
        projectName="api-server"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );
    await flushFocus();
    return { onConfirm, onCancel };
  }

  it("names what will be lost rather than just asking to confirm", async () => {
    await renderModal();
    // The whole point of the gate: Reset deletes the volumes, and the two
    // losses users do not expect are the login and the session transcripts.
    expect(screen.getByText(/sign in again/i)).toBeInTheDocument();
    expect(screen.getByText(/session transcript/i)).toBeInTheDocument();
    // And it must say what is safe, or the warning reads as "you lose everything".
    expect(screen.getByText(/mounted project folders/i)).toBeInTheDocument();
  });

  it("does not reset until confirmed", async () => {
    const { onConfirm, onCancel } = await renderModal();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("resets on confirm", async () => {
    const { onConfirm } = await renderModal();
    fireEvent.click(screen.getByRole("button", { name: "Reset container" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });
});
