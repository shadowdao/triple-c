import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import Modal from "./Modal";
import { PaneVisibilityProvider } from "./PaneVisibility";
import { dropIsBlocked } from "../../lib/dropTarget";

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

  // -------------------------------------------------------------------------
  // Stepping aside with the pane that owns it
  // -------------------------------------------------------------------------

  it("marks its backdrop as swallowing native file drops", async () => {
    // `lib/dropTarget` refuses every drop in the window while a dialog is on
    // screen, and this marker is half of how it knows one is (the panel's
    // `aria-modal` is the other half).
    render(
      <Modal title="Reset" onClose={vi.fn()}>
        <p>body</p>
      </Modal>,
    );
    const backdrop = document.querySelector(".fixed.inset-0");
    expect(backdrop).toHaveAttribute("data-blocks-drop", "true");
    expect(dropIsBlocked()).toBe(true);
  });

  it("paints nothing, traps nothing and blocks no drop while its pane is hidden", async () => {
    // A dialog portals to `document.body`, where the `hidden` class its pane
    // uses to step aside for another tab cannot reach it. Left to itself it
    // stayed on screen over the tab the user switched to, kept its Escape
    // binding, and refused every native file drop in the window.
    const onClose = vi.fn();
    const { rerender } = render(
      <PaneVisibilityProvider visible={false}>
        <Modal title="Reset" onClose={onClose}>
          <button>Confirm</button>
        </Modal>
      </PaneVisibilityProvider>,
    );
    await flushFocus();

    const backdrop = document.querySelector(".fixed.inset-0") as HTMLElement;
    expect(backdrop.hidden).toBe(true);
    expect(backdrop.style.display).toBe("none");
    expect(dropIsBlocked()).toBe(false);
    // Two independent reasons it does not block, because they are maintained
    // in two files: the marker is gone *and* `[hidden]` disqualifies it.
    expect(backdrop.hasAttribute("data-blocks-drop")).toBe(false);
    expect(backdrop.contains(document.activeElement)).toBe(false);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();

    // Back on screen: the same dialog, still mounted, resumes everything.
    // (Including the marker: it is dropped on hide, not written once at mount.)
    rerender(
      <PaneVisibilityProvider visible={true}>
        <Modal title="Reset" onClose={onClose}>
          <button>Confirm</button>
        </Modal>
      </PaneVisibilityProvider>,
    );
    await flushFocus();
    expect(backdrop.hidden).toBe(false);
    expect(dropIsBlocked()).toBe(true);
    expect(screen.getByRole("dialog").contains(document.activeElement)).toBe(true);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not leave focus inside itself when its pane steps aside", async () => {
    // The dialog goes `display:none` with the keyboard focus still inside it,
    // and nothing else relocates it — so the user arrives on the tab they
    // switched to with focus held by a dialog they cannot see, Tab resuming
    // from inside it. jsdom does not blur on `display:none` either, so this
    // is exactly the state the assertion below describes.
    const { rerender } = render(
      <PaneVisibilityProvider visible={true}>
        <Modal title="Reset" onClose={vi.fn()}>
          <button>Confirm</button>
        </Modal>
      </PaneVisibilityProvider>,
    );
    await flushFocus();
    const panel = screen.getByRole("dialog");
    expect(panel.contains(document.activeElement)).toBe(true);

    rerender(
      <PaneVisibilityProvider visible={false}>
        <Modal title="Reset" onClose={vi.fn()}>
          <button>Confirm</button>
        </Modal>
      </PaneVisibilityProvider>,
    );
    await flushFocus();
    expect(panel.contains(document.activeElement)).toBe(false);
    expect(document.activeElement).toBe(document.body);

    // …and coming back puts it where it was: inside the dialog.
    rerender(
      <PaneVisibilityProvider visible={true}>
        <Modal title="Reset" onClose={vi.fn()}>
          <button>Confirm</button>
        </Modal>
      </PaneVisibilityProvider>,
    );
    await flushFocus();
    expect(screen.getByRole("dialog").contains(document.activeElement)).toBe(true);
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
