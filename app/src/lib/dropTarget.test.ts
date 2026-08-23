import { describe, expect, it, beforeEach, afterEach } from "vitest";
import { dropIsBlocked, isDropTarget } from "./dropTarget";

function pane(rect: Partial<DOMRect>): HTMLElement {
  const el = document.createElement("div");
  document.body.appendChild(el);
  el.getBoundingClientRect = () =>
    ({
      left: 0,
      top: 0,
      right: 100,
      bottom: 100,
      width: 100,
      height: 100,
      x: 0,
      y: 0,
      toJSON: () => ({}),
      ...rect,
    }) as DOMRect;
  return el;
}

describe("dropTarget", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("accepts a point inside the pane", () => {
    expect(isDropTarget(pane({}), { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe(true);
  });

  it("rejects a point outside the pane", () => {
    expect(isDropTarget(pane({}), { x: 400, y: 50 }, { devicePixelRatio: 1 })).toBe(false);
  });

  it("converts physical pixels to CSS pixels", () => {
    const el = pane({});
    expect(isDropTarget(el, { x: 150, y: 150 }, { devicePixelRatio: 2 })).toBe(true);
    expect(isDropTarget(el, { x: 150, y: 150 }, { devicePixelRatio: 1 })).toBe(false);
  });

  it("rejects a hidden pane, which has a zero-size rect", () => {
    const el = pane({ right: 0, bottom: 0, width: 0, height: 0 });
    expect(isDropTarget(el, { x: 0, y: 0 }, { devicePixelRatio: 1 })).toBe(false);
  });

  it("rejects every drop while a modal is open", () => {
    const el = pane({});
    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    document.body.appendChild(dialog);

    expect(dropIsBlocked()).toBe(true);
    expect(isDropTarget(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe(false);

    dialog.remove();
    expect(dropIsBlocked()).toBe(false);
    expect(isDropTarget(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe(true);
  });

  it("rejects every drop while a blocking overlay is up", () => {
    const el = pane({});
    const overlay = document.createElement("div");
    overlay.setAttribute("data-blocks-drop", "true");
    document.body.appendChild(overlay);
    expect(isDropTarget(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe(false);
  });

  it("rejects a null pane", () => {
    expect(isDropTarget(null, { x: 1, y: 1 }, { devicePixelRatio: 1 })).toBe(false);
  });
});
