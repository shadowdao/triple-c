import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import Modal from "./Modal";

/**
 * Modal focuses asynchronously via rAF so the panel is laid out first; jsdom
 * needs that flushed manually.
 */
async function flushFocus() {
  await act(async () => {
    vi.advanceTimersByTime(20);
  });
}

describe("Modal", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "setTimeout"] });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("exposes dialog semantics and an accessible name", async () => {
    render(
      <Modal title="Remove Project" onClose={vi.fn()}>
        <p>body</p>
      </Modal>,
    );
    const dialog = screen.getByRole("dialog", { name: "Remove Project" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
  });

  it("moves focus into the dialog on open", async () => {
    render(
      <Modal title="Dialog" onClose={vi.fn()}>
        <button>First</button>
        <button>Second</button>
      </Modal>,
    );
    await flushFocus();
    const dialog = screen.getByRole("dialog");
    expect(dialog.contains(document.activeElement)).toBe(true);
  });

  it("traps Tab inside the dialog, wrapping at both ends", async () => {
    render(
      <Modal title="Dialog" onClose={vi.fn()} hideCloseButton>
        <button>First</button>
        <button>Last</button>
      </Modal>,
    );
    await flushFocus();

    const first = screen.getByRole("button", { name: "First" });
    const last = screen.getByRole("button", { name: "Last" });

    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(first);

    first.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("restores focus to the trigger on unmount", async () => {
    const trigger = document.createElement("button");
    document.body.appendChild(trigger);
    trigger.focus();

    const { unmount } = render(
      <Modal title="Dialog" onClose={vi.fn()}>
        <button>Inside</button>
      </Modal>,
    );
    await flushFocus();
    expect(document.activeElement).not.toBe(trigger);

    unmount();
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });

  it("closes on Escape and on an overlay click", async () => {
    const onClose = vi.fn();
    const { container } = render(
      <Modal title="Dialog" onClose={onClose}>
        <p>body</p>
      </Modal>,
    );
    await flushFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    // The overlay is the portal root's only child.
    const overlay = document.querySelector(".fixed.inset-0");
    expect(overlay).not.toBeNull();
    fireEvent.click(overlay!);
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(container).toBeTruthy();
  });

  it("ignores Escape and overlay clicks when not dismissible", async () => {
    const onClose = vi.fn();
    render(
      <Modal title="Installing" onClose={onClose} dismissible={false}>
        <p>body</p>
      </Modal>,
    );
    await flushFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.click(document.querySelector(".fixed.inset-0")!);
    expect(onClose).not.toHaveBeenCalled();
  });
});
