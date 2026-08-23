import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, fireEvent, cleanup, act } from "@testing-library/react";
import TerminalView, { supersedes } from "./TerminalView";
import { useAppState } from "../../store/appState";
import { uploadHostFileToTerminal } from "../../lib/tauri-commands";

/**
 * The window-wide native drag-drop listener, captured at registration.
 *
 * Tauri routes *every* file drop to *every* listener, which is the whole reason
 * `TerminalView` hit-tests one — so a test that wants to know what the hit test
 * decides has to be able to fire the event itself.
 */
const dragDrop = vi.hoisted(() => ({
  handler: null as null | ((event: unknown) => unknown),
}));

/** The `terminal-output-{id}` listeners, so a test can be the PTY. */
const ptyOutput = vi.hoisted(() => ({
  listeners: new Map<string, (e: { payload: number[] }) => void>(),
}));

/**
 * Shift+Enter has to reach the container as ESC+CR.
 *
 * xterm.js does not consult `shiftKey` for Enter, so Shift+Enter is
 * byte-identical to Enter unless `attachCustomKeyEventHandler` intervenes —
 * which means the interesting assertion is not just "ESC+CR was sent" but
 * "and a bare CR was not", i.e. that the handler returned false and xterm
 * stopped. A test that only checked the first half would pass on a version
 * that submits the prompt *and* inserts a newline.
 */

const terminalInput = vi.fn(async () => {});

vi.mock("../../lib/tauri-commands", () => ({
  terminalInput: (sessionId: string, bytes: number[]) =>
    terminalInput(sessionId, bytes),
  terminalResize: vi.fn(async () => {}),
  pasteImageToTerminal: vi.fn(async () => ""),
  openTerminalSession: vi.fn(async () => {}),
  closeTerminalSession: vi.fn(async () => {}),
  updateProject: vi.fn(async () => ({})),
  awsSsoRefresh: vi.fn(async () => {}),
  openPageInContainerBrowser: vi.fn(async () => ({ error: null })),
  uploadHostFileToTerminal: vi.fn(async () => ""),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, cb: (e: { payload: number[] }) => void) => {
    ptyOutput.listeners.set(event, cb);
    return () => ptyOutput.listeners.delete(event);
  },
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (cb: (event: unknown) => unknown) => {
      dragDrop.handler = cb;
      return () => {
        dragDrop.handler = null;
      };
    },
  }),
}));

/** jsdom has no ResizeObserver, and the mount effect installs one. */
class NoopResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

/** What `sendInput` put on the wire, decoded back to a string. */
function sent(): string[] {
  return terminalInput.mock.calls.map((call) =>
    new TextDecoder().decode(new Uint8Array((call as unknown as [string, number[]])[1])),
  );
}

function mountSession(sessionType: "claude" | "bash") {
  useAppState.setState({
    sessions: [
      {
        id: "s1",
        projectId: "p1",
        projectName: "api",
        sessionType,
        sessionName: null,
      },
    ],
  });
  return render(<TerminalView sessionId="s1" active />);
}

/** The hidden textarea xterm binds its keyboard handling to. */
function helperTextarea(container: HTMLElement): HTMLTextAreaElement {
  const el = container.querySelector<HTMLTextAreaElement>(
    "textarea.xterm-helper-textarea",
  );
  if (!el) throw new Error("xterm helper textarea not found");
  return el;
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", NoopResizeObserver);
  // xterm's renderer asks the window for its device pixel ratio on open.
  vi.stubGlobal(
    "matchMedia",
    (query: string) => ({
      matches: false,
      media: query,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
      onchange: null,
      dispatchEvent: () => false,
    }),
  );
  terminalInput.mockClear();
  vi.mocked(uploadHostFileToTerminal).mockClear();
  vi.mocked(uploadHostFileToTerminal).mockResolvedValue("/workspace/api/dropped.txt");
  dragDrop.handler = null;
  ptyOutput.listeners.clear();
  document.body.innerHTML = "";
  useAppState.setState({ sessions: [] });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("TerminalView — Shift+Enter", () => {
  it("sends ESC+CR and nothing else in a Claude session", () => {
    const { container } = mountSession("claude");

    fireEvent.keyDown(helperTextarea(container), {
      key: "Enter",
      keyCode: 13,
      shiftKey: true,
    });

    // The bytes `/terminal-setup` installs for every other editor.
    expect(sent()).toEqual(["\x1b\r"]);
    // And specifically not the bare CR that would have submitted the prompt.
    expect(sent()).not.toContain("\r");
  });

  it("leaves a plain Enter alone", () => {
    const { container } = mountSession("claude");

    fireEvent.keyDown(helperTextarea(container), { key: "Enter", keyCode: 13 });

    expect(sent()).toEqual(["\r"]);
  });

  it("does not bind it in a bash session", () => {
    // `bash -l` runs readline, which has no binding for `\e\r`: it would answer
    // with a bell and swallow the Enter the user actually pressed.
    const { container } = mountSession("bash");

    fireEvent.keyDown(helperTextarea(container), {
      key: "Enter",
      keyCode: 13,
      shiftKey: true,
    });

    expect(sent()).toEqual(["\r"]);
  });

  it("leaves a modified Shift+Enter to xterm", () => {
    // Adding Ctrl is not the chord this binds; whatever xterm does with it is
    // xterm's business.
    const { container } = mountSession("claude");

    fireEvent.keyDown(helperTextarea(container), {
      key: "Enter",
      keyCode: 13,
      shiftKey: true,
      ctrlKey: true,
    });

    expect(sent()).not.toContain("\x1b\r");
  });

  it("Alt+Enter already produced ESC+CR without any handler", () => {
    // Pinned because it is the reason Shift+Enter was the only gap: xterm
    // ESC-prefixes on `altKey` by itself, so Alt+Enter has always inserted a
    // newline in Claude Code. It was simply undocumented.
    const { container } = mountSession("bash"); // no custom branch involved

    fireEvent.keyDown(helperTextarea(container), {
      key: "Enter",
      keyCode: 13,
      altKey: true,
    });

    expect(sent()).toEqual(["\x1b\r"]);
  });
});

describe("supersedes — who owns the prompt slot", () => {
  const relay = (url: string) => ({ url, source: "relay" as const });
  const osc8 = (url: string) => ({ url, source: "osc8" as const });
  const guess = (url: string) => ({ url, source: "heuristic" as const });

  const COMPLETE =
    "https://claude.ai/oauth/authorize?code=true&client_id=abc123&response_type=code&redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference";
  // What the screen-scraper reconstructs from the visible text: parses, points
  // at the right host, authorises nothing.
  const TRUNCATED = COMPLETE.slice(0, 80);

  it("fills an empty slot from anywhere", () => {
    expect(supersedes(guess(TRUNCATED), null)).toBe(true);
  });

  it("refuses to let a truncated guess replace the exact copy", () => {
    // The whole bug: the relay lands first with the complete URL, and 300 ms
    // later the detector's debounce fires with a prefix of it.
    expect(supersedes(guess(TRUNCATED), relay(COMPLETE))).toBe(false);
    expect(supersedes(guess(TRUNCATED), osc8(COMPLETE))).toBe(false);
  });

  it("lets a better source take over from a worse one", () => {
    expect(supersedes(osc8(COMPLETE), guess(TRUNCATED))).toBe(true);
    expect(supersedes(relay(COMPLETE), guess(TRUNCATED))).toBe(true);
  });

  it("refuses to let a truncated guess replace another guess it truncates", () => {
    // The same rule one rank down. Both are scrapes of the same repainting
    // frame, so recency says the newer one wins and recency is wrong: a
    // repaint that lands a *shorter* view of the link already on screen is
    // showing less of it, not something new.
    expect(supersedes(guess(TRUNCATED), guess(COMPLETE))).toBe(false);
  });

  it("lets a scraped candidate grow into the complete link", () => {
    // A repaint can land the truncated copy first. Extending it is safe: a
    // longer string with the same prefix has the same origin.
    expect(supersedes(guess(COMPLETE), guess(TRUNCATED))).toBe(true);
  });

  it("does not let an unrelated scrape displace what is on screen", () => {
    // Longest-wins without the prefix test hands the choice to whoever pads
    // their URL the most.
    expect(
      supersedes(guess("https://evil.tld/" + "a".repeat(400)), guess(COMPLETE)),
    ).toBe(false);
  });

  it("lets a second explicit relay request through", () => {
    // Each OSC 7777 is a fresh deliberate ask, not another view of the last
    // one — a second `gh auth login` must be able to replace the first.
    expect(
      supersedes(relay("https://github.com/login/device"), relay(COMPLETE)),
    ).toBe(true);
  });
});

describe("TerminalView — where a dropped file lands", () => {
  /** Mount, let the async drag-drop registration settle, and give the pane a
   *  rect — jsdom has no layout, so every element is 0×0 and would be rejected
   *  as a hidden pane.
   *
   *  The rect goes on the *pane wrapper*, which is what the hit test asks
   *  about: it is what the user sees as the terminal (gutter included), and
   *  the chrome painted over it — the Following toggle, the URL toast — are
   *  its children rather than the xterm host's. */
  async function mountWithLayout() {
    const view = mountSession("bash");
    await act(async () => {});
    const pane = view.container.firstElementChild as HTMLElement | null;
    if (!pane) throw new Error("terminal pane not found");
    pane.getBoundingClientRect = () =>
      ({
        left: 0,
        top: 0,
        right: 800,
        bottom: 600,
        width: 800,
        height: 600,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      }) as DOMRect;
    return view;
  }

  /** jsdom has no `elementFromPoint`, so a z-order branch is unreachable
   *  unless a test supplies one — which is exactly how a gate that refused
   *  every drop under the Following toggle shipped through this file green.
   *  The gate asks no per-point question any more, but the tests below still
   *  install one and feed it the most misleading answer available, to pin
   *  that the routing does not change when it is there. */
  function stubElementFromPoint(top: Element | null) {
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      writable: true,
      value: () => top,
    });
  }

  async function drop(x: number, y: number) {
    if (!dragDrop.handler) throw new Error("no drag-drop listener registered");
    await act(async () => {
      await dragDrop.handler!({
        payload: { type: "drop", position: { x, y }, paths: ["/host/dropped.txt"] },
      });
    });
  }

  it("uploads a file dropped onto the pane", async () => {
    await mountWithLayout();
    await drop(400, 300);
    expect(vi.mocked(uploadHostFileToTerminal)).toHaveBeenCalledWith(
      "s1",
      "/host/dropped.txt",
    );
  });

  it("ignores a drop that lands outside the pane", async () => {
    await mountWithLayout();
    await drop(4000, 300);
    expect(vi.mocked(uploadHostFileToTerminal)).not.toHaveBeenCalled();
  });

  it("ignores a drop released onto an open modal", async () => {
    // The hit test used to be purely geometric, and a `Modal` is a
    // `fixed inset-0 z-50` portal painted *over* the whole window — so the pane
    // underneath still had its rect and happily uploaded the file into the
    // directory the dialog was covering. Same for the shutdown overlay, which is
    // up precisely while nothing should be accepting work.
    await mountWithLayout();
    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    document.body.appendChild(dialog);

    await drop(400, 300);

    expect(vi.mocked(uploadHostFileToTerminal)).not.toHaveBeenCalled();

    // …and it is the modal, not the mount, that is refusing: close it and the
    // very same drop goes through.
    dialog.remove();
    await drop(400, 300);
    expect(vi.mocked(uploadHostFileToTerminal)).toHaveBeenCalledTimes(1);
  });

  it("uploads a file dropped onto the always-present Following toggle", async () => {
    // The regression this file could not see. The toggle is `absolute top-2
    // right-4 z-50` and is rendered unconditionally, so `elementFromPoint`
    // returns *it* for the terminal's top-right corner — and a gate asking
    // "is what is painted here inside the xterm host?" answered no, forever,
    // with no message and no log line. jsdom never ran that branch.
    const view = await mountWithLayout();
    const toggle = view.getByTitle(/Auto-scroll/i);
    stubElementFromPoint(toggle);

    await drop(780, 10);

    expect(vi.mocked(uploadHostFileToTerminal)).toHaveBeenCalledWith(
      "s1",
      "/host/dropped.txt",
    );
    delete (document as Partial<Document>).elementFromPoint;
  });

  it("refuses — and says so — while a dialog is open", async () => {
    useAppState.setState({ toasts: [] });
    await mountWithLayout();
    const backdrop = document.createElement("div");
    backdrop.setAttribute("data-blocks-drop", "true");
    const panel = document.createElement("div");
    panel.setAttribute("aria-modal", "true");
    backdrop.appendChild(panel);
    document.body.appendChild(backdrop);
    stubElementFromPoint(backdrop);

    await drop(400, 300);

    expect(vi.mocked(uploadHostFileToTerminal)).not.toHaveBeenCalled();
    // A refused drop is otherwise indistinguishable from a broken one.
    const notice = useAppState
      .getState()
      .toasts.find((t) => t.message === "File drop ignored");
    expect(notice).toBeTruthy();
    // Not an error: the user has a dialog open, which is a state they chose
    // and can leave with Escape. An error card would sit there until
    // dismissed, and `ToastHost` paints at `z-[60]`.
    expect(notice?.kind).toBe("info");

    backdrop.remove();
    delete (document as Partial<Document>).elementFromPoint;
  });

  it("keeps refusing when the refusal's own toast is painted over the dialog", async () => {
    // C1, end to end. Refusing pushes a toast; `ToastHost` is `fixed
    // bottom-4 right-4 z-[60]` and the `Modal` backdrop is `z-50` in the same
    // stacking context — so the toast is the topmost element over the covered
    // pane, and a gate that asked "is a blocker painted here?" answered no and
    // uploaded into the directory the dialog was covering. The gate had armed
    // its own hole: one refused drop was all it took to open it.
    useAppState.setState({ toasts: [] });
    await mountWithLayout();
    const backdrop = document.createElement("div");
    backdrop.setAttribute("data-blocks-drop", "true");
    document.body.appendChild(backdrop);
    stubElementFromPoint(backdrop);

    await drop(400, 300);
    expect(vi.mocked(uploadHostFileToTerminal)).not.toHaveBeenCalled();
    expect(useAppState.getState().toasts).toHaveLength(1);

    // The toast is now on screen, above the backdrop, and the user drops again
    // on the very point it occupies.
    const toastCard = document.createElement("div");
    document.body.appendChild(toastCard);
    stubElementFromPoint(toastCard);

    await drop(700, 550);
    expect(vi.mocked(uploadHostFileToTerminal)).not.toHaveBeenCalled();
    // …and a second refusal replaces the first notice rather than stacking.
    expect(useAppState.getState().toasts).toHaveLength(1);

    toastCard.remove();
    backdrop.remove();
    delete (document as Partial<Document>).elementFromPoint;
  });
});

describe("TerminalView — reaching the URL prompt without a mouse", () => {
  // This toast is the only route to completing a sign-in started in a terminal.
  // It used to be mouse-only: nothing moved focus to it, nothing dismissed it
  // from the keyboard, and xterm's helper textarea eats Tab, so its buttons
  // could not be reached at all.
  const SIGN_IN =
    "https://claude.ai/oauth/authorize?code=true&client_id=abc&response_type=code";

  /** What `container/triple-c-open` writes to its controlling terminal. */
  function relaySequence(url: string): number[] {
    const payload = btoa(url);
    return Array.from(
      new TextEncoder().encode(`\x1b]7777;open;${payload}\x07`),
    );
  }

  /** Mount, and let the container ask for a URL to be opened. */
  async function mountWithPrompt() {
    const view = mountSession("claude");
    await act(async () => {});
    const emit = ptyOutput.listeners.get("terminal-output-s1");
    if (!emit) throw new Error("no terminal-output listener registered");
    await act(async () => {
      emit({ payload: relaySequence(SIGN_IN) });
      // xterm parses on its own write queue.
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });
    return view;
  }

  function primaryAction(): HTMLElement {
    const el = document.querySelector<HTMLElement>('[data-url-toast-primary="true"]');
    if (!el) throw new Error("toast default action not found");
    return el;
  }

  it("does not take focus away from the terminal when the prompt appears", async () => {
    // Deliberate: the terminal is live, and the default action opens a URL the
    // *container* chose. A focused button is one stray Enter from doing it.
    const { container } = await mountWithPrompt();
    expect(document.querySelector('[data-testid="url-toast"]')).not.toBeNull();
    expect(document.activeElement).toBe(helperTextarea(container));
  });

  it("jumps to the default action on Ctrl+Shift+O", async () => {
    const { container } = await mountWithPrompt();

    fireEvent.keyDown(helperTextarea(container), {
      key: "O",
      ctrlKey: true,
      shiftKey: true,
    });

    expect(document.activeElement).toBe(primaryAction());
  });

  it("dismisses on Escape and hands focus back to the terminal", async () => {
    // Not back to `document.body`, where the next keystroke goes nowhere.
    const { container } = await mountWithPrompt();
    fireEvent.keyDown(helperTextarea(container), {
      key: "O",
      ctrlKey: true,
      shiftKey: true,
    });

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });

    expect(document.querySelector('[data-testid="url-toast"]')).toBeNull();
    expect(document.activeElement).toBe(helperTextarea(container));
  });

  it("leaves Ctrl+Shift+O to the terminal when there is no prompt", async () => {
    const { container } = mountSession("claude");
    await act(async () => {});
    const before = document.activeElement;

    fireEvent.keyDown(helperTextarea(container), {
      key: "O",
      ctrlKey: true,
      shiftKey: true,
    });

    expect(document.activeElement).toBe(before);
  });
});
