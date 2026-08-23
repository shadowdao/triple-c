import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
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
 * through 81 drop tests: the branch was never entered, in any of them.
 *
 * The gate no longer asks a per-point question at all, so the gap can no
 * longer hide anything here — but the tests below still *install* an
 * `elementFromPoint` and hand it the most misleading answer available, because
 * "the module ignores it" is now a property worth pinning. `spy.mock.calls`
 * proves it directly.
 */
function stubElementFromPoint(top: Element | null): ReturnType<typeof vi.fn> {
  const spy = vi.fn(() => top);
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    writable: true,
    value: spy,
  });
  return spy;
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
  // The gate itself: document-wide, and provably not geometric
  // ---------------------------------------------------------------------

  describe("the blocking gate is document-wide, with no z-order in it", () => {
    it("never consults elementFromPoint, however tempting its answer", () => {
      // Round 2 asked `elementFromPoint` whether a *blocker* was painted at
      // the drop point, and trusted the answer absolutely. Anything painted
      // above the `z-50` backdrop in the same stacking context — `ToastHost`
      // at `z-[60]`, `TerminalContextMenu` at `z-[60]` — answered "no blocker
      // here" on a point the dialog was covering. The fix is not a better
      // answer, it is not asking: this pins that the call is gone, so a
      // future edit that reintroduces it fails here rather than in the wild.
      const el = pane({});
      const child = document.createElement("span");
      el.appendChild(child);
      const spy = stubElementFromPoint(child);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
      expect(spy).not.toHaveBeenCalled();
    });

    it("refuses a covered drop even when a toast is painted over the dialog", () => {
      // C1, exactly. A refused drop pushes a toast; `ToastHost` is
      // `fixed bottom-4 right-4 z-[60]` and an error card stays until
      // dismissed; the *next* drop released on that card had a topmost
      // element with no blocker in it, and landed in the directory the dialog
      // was covering. The gate had armed its own hole.
      const el = pane({});
      openModal(); // z-50 backdrop, covering the pane
      const toastCard = document.createElement("div"); // z-[60], over the backdrop
      document.body.appendChild(toastCard);
      const spy = stubElementFromPoint(toastCard);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
      expect(spy).not.toHaveBeenCalled();
    });

    it("refuses a drop anywhere in the window while a dialog is open", () => {
      // Deliberately stricter than "the points the dialog covers". A dialog
      // is a state the user entered on purpose and leaves with Escape, the
      // refusal is announced, and nothing is written — whereas being precise
      // about coverage has silently uploaded into a covered directory twice.
      const el = pane({});
      const { backdrop } = openModal();
      stubElementFromPoint(document.createElement("div")); // "nothing here"

      expect(classifyDrop(el, { x: 5, y: 5 }, { devicePixelRatio: 1 })).toBe("blocked");
      expect(classifyDrop(el, { x: 95, y: 95 }, { devicePixelRatio: 1 })).toBe("blocked");

      backdrop.remove();
      expect(classifyDrop(el, { x: 5, y: 5 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("refuses under the shutdown overlay, which is not a dialog", () => {
      const el = pane({});
      const overlay = document.createElement("div");
      overlay.setAttribute("data-blocks-drop", "true");
      document.body.appendChild(overlay);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("still refuses a dialog built without ui/Modal's marked backdrop", () => {
      // Only `aria-modal` is needed; `ui/Modal` is the supported route, but a
      // hand-rolled dialog must not be a hole either.
      const el = pane({});
      const backdrop = document.createElement("div");
      const panel = document.createElement("div");
      panel.setAttribute("aria-modal", "true");
      backdrop.appendChild(panel);
      document.body.appendChild(backdrop);

      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("counts a blocker that is only decoratively hidden from assistive tech", () => {
      // `aria-hidden="true"` used to disqualify a blocker, and it is not a
      // visibility statement — `ui/Modal`'s own ✕ glyph carries it while
      // perfectly visible. A blocker nested inside such a wrapper would have
      // silently stopped blocking. Only `[hidden]` counts now.
      const el = pane({});
      const wrapper = document.createElement("div");
      wrapper.setAttribute("aria-hidden", "true");
      const overlay = document.createElement("div");
      overlay.setAttribute("data-blocks-drop", "true");
      wrapper.appendChild(overlay);
      document.body.appendChild(wrapper);

      expect(dropIsBlocked()).toBe(true);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
    });

    it("ignores a blocker whose pane has stepped aside", () => {
      // `ui/Modal` marks itself `hidden` when the tab that owns it is not the
      // one on screen. A dialog left open in project A must not keep refusing
      // drops in project B — this is the one case where over-refusal would be
      // unbounded, since the user cannot see the dialog to close it.
      const el = pane({});
      const { backdrop } = openModal();
      backdrop.setAttribute("hidden", "");

      expect(dropIsBlocked()).toBe(false);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
    });
  });

  // ---------------------------------------------------------------------
  // Round 1's shape: chrome painted over a pane must never refuse a drop
  // ---------------------------------------------------------------------

  describe("chrome over a pane, with no dialog open", () => {
    /** Everything that is painted over a pane and is not a blocker. */
    const CHROME: Array<[string, () => HTMLElement]> = [
      // `TerminalView`'s "▼ Following / ▽ Paused" toggle: `absolute top-2
      // right-4 z-50`, rendered unconditionally, and a *sibling* of the xterm
      // host — so "does the pane contain what is painted here?" made the
      // terminal's top-right corner a dead zone no user action could clear.
      ["the Following/Paused toggle", () => document.createElement("button")],
      // `ToastHost`: `fixed bottom-4 right-4 z-[60]`, 24rem wide, over every
      // pane, and its error cards stay until dismissed.
      ["a toast card", () => document.createElement("div")],
      // The drop hint is `pointer-events-none`, so a real `elementFromPoint`
      // skips it — but nothing may depend on that any more.
      ["the pane's own drop hint", () => document.createElement("div")],
      // `Tooltip` portals to `document.body`, so it is nobody's child.
      ["a portaled tooltip", () => document.createElement("div")],
    ];

    for (const [name, make] of CHROME) {
      it(`accepts a drop released onto ${name}`, () => {
        const el = pane({});
        const chrome = make();
        document.body.appendChild(chrome); // a sibling, not a child of the pane
        stubElementFromPoint(chrome);

        expect(classifyDrop(el, { x: 90, y: 5 }, { devicePixelRatio: 1 })).toBe("accept");
        expect(classifyDrop(el, { x: 95, y: 95 }, { devicePixelRatio: 1 })).toBe("accept");
      });
    }

    it("accepts a drop on the gutter around the terminal, and on its content", () => {
      const el = pane({});
      const child = document.createElement("span");
      el.appendChild(child);
      stubElementFromPoint(child);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");

      stubElementFromPoint(el); // the gutter: the wrapper itself is topmost
      expect(classifyDrop(el, { x: 1, y: 99 }, { devicePixelRatio: 1 })).toBe("accept");
    });

    it("accepts a drop the view could not resolve to any element", () => {
      // `elementFromPoint` answers `null` outside the viewport and `<body>`
      // over nothing in particular. With no dialog open neither is a reason
      // to refuse a drop that is inside the pane's rect.
      const el = pane({});
      stubElementFromPoint(null);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
      stubElementFromPoint(document.body);
      expect(classifyDrop(el, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("accept");
    });
  });

  // ---------------------------------------------------------------------
  // Routing: exactly one listener speaks for a given drop
  // ---------------------------------------------------------------------

  describe("routing", () => {
    it("lets only the pane the drop landed on report a refusal", () => {
      // Geometry is asked before the gate for this reason: both listeners are
      // live for every drop, and if the gate came first they would both push
      // "File drop ignored" for one drop.
      const hit = pane({});
      const missed = pane({ left: 200, right: 300, top: 200, bottom: 300 });
      openModal();

      expect(classifyDrop(hit, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe("blocked");
      expect(classifyDrop(missed, { x: 50, y: 50 }, { devicePixelRatio: 1 })).toBe(
        "elsewhere",
      );
    });

    it("says elsewhere, not blocked, for a hidden pane's zero-size rect", () => {
      const hidden = pane({ right: 0, bottom: 0, width: 0, height: 0 });
      openModal();
      expect(classifyDrop(hidden, { x: 0, y: 0 }, { devicePixelRatio: 1 })).toBe(
        "elsewhere",
      );
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
