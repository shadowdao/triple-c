import { describe, expect, it, beforeEach, afterEach } from "vitest";
import { classifyDrop, dropIsBlocked, isDropTarget } from "./dropTarget";

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

/**
 * **jsdom has no `elementFromPoint`.** That single fact is how a z-order gate
 * that refused every drop under a button, a toast and a dialog shipped green
 * through 81 drop tests: the branch was never entered, in any of them. So the
 * tests below install one. A test that passes because the environment lacks
 * the API under test is not a test.
 */
function stubElementFromPoint(top: Element | null): void {
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    writable: true,
    value: () => top,
  });
}

function removeElementFromPoint(): void {
  delete (document as Partial<Document>).elementFromPoint;
}

/** The DOM `ui/Modal` really renders: a marked backdrop around the dialog. */
function openModal(): { backdrop: HTMLElement; panel: HTMLElement; button: HTMLElement } {
  const backdrop = document.createElement("div");
  backdrop.setAttribute("data-blocks-drop", "true");
  const panel = document.createElement("div");
  panel.setAttribute("role", "dialog");
  panel.setAttribute("aria-modal", "true");
  const button = document.createElement("button");
  panel.appendChild(button);
  backdrop.appendChild(panel);
  document.body.appendChild(backdrop);
  return { backdrop, panel, button };
}

function withUserAgent(ua: string): void {
  Object.defineProperty(window.navigator, "userAgent", {
    configurable: true,
    value: ua,
  });
}

const REAL_UA = window.navigator.userAgent;
const REAL_DPR = window.devicePixelRatio;

function withDevicePixelRatio(value: number): void {
  Object.defineProperty(window, "devicePixelRatio", { configurable: true, value });
}

describe("dropTarget", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });
  afterEach(() => {
    document.body.innerHTML = "";
    removeElementFromPoint();
    withUserAgent(REAL_UA);
    withDevicePixelRatio(REAL_DPR);
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

  // ---------------------------------------------------------------------
  // Z-order — the branch jsdom cannot reach on its own
  // ---------------------------------------------------------------------

  describe("z-order, with elementFromPoint actually present", () => {
    it("confirms the environment gap this whole block exists for", () => {
      // If jsdom ever grows layout, this fails and the stubs below can be
      // reconsidered — but until then, *nothing* reaches the z-order branch
      // unless a test puts the API there itself.
      expect(typeof document.elementFromPoint).toBe("undefined");
    });

    it("accepts a drop onto chrome the pane paints over itself", () => {
      // The regression. `TerminalView`'s "▼ Following / ▽ Paused" toggle is
      // `absolute top-2 right-4 z-50` and is rendered *unconditionally*, as a
      // sibling of the xterm host rather than a child — so a gate asking "does
      // the pane contain what is painted here?" turned the terminal's top-right
      // corner into a dead zone that no user action could clear.
      const el = pane({});
      const following = document.createElement("button");
      document.body.appendChild(following); // sibling, not a child of the pane
      stubElementFromPoint(following);

      expect(classifyDrop(el, { x: 90, y: 5 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("accepts a drop onto a toast floating above every pane", () => {
      // `ToastHost` is `fixed bottom-4 right-4 z-[60]`, 24rem wide, and its
      // error cards never time out — so under the containment rule the
      // bottom-right corner of both the terminal and the Files pane stopped
      // accepting drops for as long as one error stayed on screen.
      const el = pane({});
      const toastCard = document.createElement("div");
      document.body.appendChild(toastCard);
      stubElementFromPoint(toastCard);

      expect(classifyDrop(el, { x: 95, y: 95 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("accepts a drop onto the pane's own content", () => {
      const el = pane({});
      const child = document.createElement("span");
      el.appendChild(child);
      stubElementFromPoint(child);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("refuses a drop released onto a dialog's backdrop", () => {
      const el = pane({});
      const { backdrop } = openModal();
      stubElementFromPoint(backdrop);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("refuses a drop released onto the dialog panel or anything inside it", () => {
      const el = pane({});
      const { panel, button } = openModal();

      stubElementFromPoint(panel);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");

      stubElementFromPoint(button);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("refuses a drop under an unmarked overlay that wraps a dialog", () => {
      // Belt to the braces: a dialog built without `ui/Modal`'s marked
      // backdrop is still refused, because the element painted at the point
      // *contains* something modal.
      const el = pane({});
      const backdrop = document.createElement("div");
      const panel = document.createElement("div");
      panel.setAttribute("aria-modal", "true");
      backdrop.appendChild(panel);
      document.body.appendChild(backdrop);
      stubElementFromPoint(backdrop);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("refuses a drop under the shutdown overlay", () => {
      const el = pane({});
      const overlay = document.createElement("div");
      overlay.setAttribute("data-blocks-drop", "true");
      document.body.appendChild(overlay);
      stubElementFromPoint(overlay);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("scopes a dialog's refusal to the points it actually covers", () => {
      // `dropIsBlocked` is document-wide and `ui/Modal` portals to
      // `document.body`, so an open dialog used to refuse every drop in the
      // window. Asked per point, a dialog only swallows what lands on it.
      const el = pane({});
      openModal();
      const paneContent = document.createElement("span");
      el.appendChild(paneContent);
      stubElementFromPoint(paneContent);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("ignores a blocker whose pane has stepped aside", () => {
      // `ui/Modal` marks itself `hidden` when the tab that owns it is not the
      // one on screen. A dialog left open in project A must not keep refusing
      // drops in project B.
      const el = pane({});
      const { backdrop } = openModal();
      backdrop.setAttribute("hidden", "");

      expect(dropIsBlocked()).toBe(false);
      stubElementFromPoint(backdrop);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("falls back to the document-wide question when the point cannot be resolved", () => {
      // `elementFromPoint` answers `null` for a point outside the viewport, and
      // `<body>` for a point over nothing in particular. Neither is evidence
      // that the pane is clear, so the conservative answer is the old one.
      const el = pane({});
      openModal();

      stubElementFromPoint(null);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");

      stubElementFromPoint(document.body);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("tells a refused drop apart from someone else's drop", () => {
      // The caller reports one and stays silent about the other: a drop that
      // was aimed at this pane and swallowed by an overlay is invisible unless
      // something says so, while a drop into another pane is not ours to
      // narrate.
      const el = pane({});
      const { backdrop } = openModal();
      stubElementFromPoint(backdrop);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
      expect(classifyDrop(el, { x: 900, y: 900 }, { devicePixelRatio: 1 })).toBe("elsewhere");
    });
  });

  // ---------------------------------------------------------------------
  // HiDPI
  // ---------------------------------------------------------------------

  describe("physical vs logical payload coordinates", () => {
    it("divides by devicePixelRatio on Windows", () => {
      // wry's WebView2 drag-drop handler passes the OS point through in device
      // pixels, so a physical (150,150) at dpr 2 is a CSS (75,75).
      withUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36");
      withDevicePixelRatio(2);
      expect(isDropTarget(pane({}), { x: 150, y: 150 })).toBe(true);
    });

    it("does not divide on macOS or Linux, where the payload is already logical", () => {
      // The macOS and GTK backends deliver logical points and
      // `tauri-runtime-wry` does not rescale them. Halving one there aimed the
      // hit test at a point the user never touched — harmless while the test
      // was a bare rect, and a refused drop once z-order joined in.
      withDevicePixelRatio(2);

      withUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15");
      expect(isDropTarget(pane({}), { x: 150, y: 150 })).toBe(false);
      expect(isDropTarget(pane({}), { x: 50, y: 50 })).toBe(true);

      withUserAgent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36");
      expect(isDropTarget(pane({}), { x: 150, y: 150 })).toBe(false);
      expect(isDropTarget(pane({}), { x: 50, y: 50 })).toBe(true);
    });

    it("takes an explicit override over the platform guess", () => {
      withUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15");
      withDevicePixelRatio(2);
      expect(
        isDropTarget(pane({}), { x: 150, y: 150 }, { physicalPixelPayload: true }),
      ).toBe(true);
    });
  });
});
