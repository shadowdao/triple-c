import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, fireEvent, cleanup, act } from "@testing-library/react";
import TerminalView, {
  OSC8_HOVER_CLASS,
  createOsc8LinkHandler,
  supersedes,
} from "./TerminalView";
import { useAppState } from "../../store/appState";
import {
  uploadHostFileToTerminal,
  openUrlExternal,
} from "../../lib/tauri-commands";
import {
  chooseSignInTarget,
  resetBrowserSupportCache,
} from "../../hooks/useSignInOpenTarget";
import type { AuthBridgeStatus, PlaywrightDetection } from "../../lib/types";
import { URL_TOAST_SELECTOR } from "./UrlToast";

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

/**
 * What the project's container answers about itself.
 *
 * `TerminalView` asks two questions on mount — is the auth bridge live, and is
 * there a browser inside to open a page in — because together they decide which
 * of the URL toast's two buttons leads for a sign-in link.
 */
const containerEnv = vi.hoisted(() => ({
  bridge: { enabled: false, active_ports: [], conflicts: [] } as unknown,
  detection: null as unknown,
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
  getAuthBridgeStatus: vi.fn(async () => containerEnv.bridge),
  checkBrowserViewSupport: vi.fn(async () => containerEnv.detection),
  openUrlExternal: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, cb: (e: { payload: number[] }) => void) => {
    ptyOutput.listeners.set(event, cb);
    return () => ptyOutput.listeners.delete(event);
  },
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
  vi.mocked(openUrlExternal).mockReset();
  vi.mocked(openUrlExternal).mockResolvedValue(undefined);
  containerEnv.bridge = { enabled: false, active_ports: [], conflicts: [] };
  containerEnv.detection = null;
  // The Playwright probe is memoized across mounts (it is a container exec), so
  // a case that changes the answer has to drop what an earlier one cached.
  resetBrowserSupportCache();
  useAppState.setState({ toasts: [] });
  document.body.innerHTML = "";
  useAppState.setState({ sessions: [] });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("TerminalView — Shift+Enter", () => {
  it("sends ESC+CR and cancels the keydown, so no bare CR follows", () => {
    // **The cancel is the load-bearing half, and this test could not see it.**
    //
    // Returning `false` from xterm's custom key handler does not cancel the
    // event: `_keyDown` returns before setting `_keyDownHandled`, so
    // `_keyPress` still runs and emits a bare CR for Enter's charCode 13. In a
    // real browser that submitted the prompt straight after inserting the
    // newline. jsdom never synthesizes the follow-up keypress, so the old
    // `expect(sent()).not.toContain("\r")` assertion below could not fail no
    // matter what the code did — it was named for a behaviour it could not
    // exercise.
    //
    // Asserting `defaultPrevented` pins the actual mechanism that stops the
    // keypress, which is a property jsdom *can* observe.
    const { container } = mountSession("claude");

    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      keyCode: 13,
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });
    helperTextarea(container).dispatchEvent(event);

    // The bytes `/terminal-setup` installs for every other editor.
    expect(sent()).toEqual(["\x1b\r"]);
    expect(sent()).not.toContain("\r");
    // Without this, the browser fires keypress and xterm submits.
    expect(event.defaultPrevented).toBe(true);
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

  it("uploads a file dropped onto the chrome painted over the terminal", async () => {
    // The regression this file could not see. Chrome like the URL toast is a
    // *sibling* of the xterm host painted over the pane, so
    // `elementFromPoint` returns it rather than the host — and a gate asking
    // "is what is painted here inside the xterm host?" answered no, forever,
    // with no message and no log line. jsdom never ran that branch.
    //
    // The original fixture was the always-rendered "▼ Following" toggle. That
    // control is retired and the mouse-release button that could have replaced
    // it lives in the status bar now, so the toast is what stands in — it is
    // real chrome over the pane, which is the only property under test.
    await mountWithLayout();
    const emit = ptyOutput.listeners.get("terminal-output-s1");
    if (!emit) throw new Error("no terminal-output listener registered");
    await act(async () => {
      emit({
        payload: Array.from(
          new TextEncoder().encode(
            `\x1b]7777;open;${btoa("https://example.com/x")}\x07`,
          ),
        ),
      });
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });
    const toast = document.querySelector(URL_TOAST_SELECTOR);
    if (!toast) throw new Error("URL toast not shown");
    stubElementFromPoint(toast);

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

/**
 * A container with Playwright *and* a browser in the cache — i.e. one where
 * "In container" would actually open something.
 */
function usableDetection(
  over: Partial<PlaywrightDetection> = {},
): PlaywrightDetection {
  return {
    node_version: "v22.11.0",
    playwright_version: "1.56.0",
    playwright_path: "/workspace/node_modules/playwright",
    playwright_cli: "/workspace/node_modules/playwright/cli.js",
    has_bind: true,
    cli_version: "1.56.0",
    cli_entry: "/workspace/node_modules/@playwright/cli/index.js",
    browsers: ["chromium-1200"],
    chrome_channel: null,
    chromium_executable: "/home/claude/.cache/ms-playwright/chromium-1200/chrome",
    chromium_executable_exists: true,
    script_playwright_version: "1.56.0",
    script_chromium_executable: null,
    script_chromium_executable_exists: false,
    searched: [],
    ...over,
  };
}

const LIVE_BRIDGE: AuthBridgeStatus = {
  enabled: true,
  active_ports: [],
  conflicts: [],
};

describe("chooseSignInTarget — which action leads for a sign-in link", () => {
  // The rule this replaced was "container, always", justified by the callback
  // listener living inside the container. Both halves of that justification
  // stopped being true: the auth bridge mirrors that listener onto the host,
  // and the container-side target is Playwright's pane, whose browsers are not
  // in the image.
  it("prefers the host browser whenever the bridge is live", () => {
    expect(chooseSignInTarget(LIVE_BRIDGE, usableDetection())).toBe("host-bridged");
  });

  it("does not call a bridge live while it is holding a port conflict", () => {
    // Enabled and unable to catch the callback anyway — the one state where
    // "on" must not read as "will work".
    const conflicted: AuthBridgeStatus = {
      enabled: true,
      active_ports: [],
      conflicts: [{ port: 54545, reason: "already in use on the host" }],
    };
    expect(chooseSignInTarget(conflicted, usableDetection())).toBe("container");
  });

  it("does not wait for a bridged port before trusting an enabled bridge", () => {
    // There is nothing to bridge until the CLI binds its listener, and that
    // races the URL reaching the transcript. Requiring a port would make the
    // default flip between two identical sign-ins.
    expect(chooseSignInTarget(LIVE_BRIDGE, null)).toBe("host-bridged");
  });

  it("falls to the container only when it has a browser to open", () => {
    const off: AuthBridgeStatus = { enabled: false, active_ports: [], conflicts: [] };
    expect(chooseSignInTarget(off, usableDetection())).toBe("container");
    // Not plain "host": with the bridge off and no browser inside, nothing is
    // carrying the callback, and the toast's hint has to say so rather than
    // promising a bridge. That distinction is the whole reason this answer is
    // three-valued.
    expect(chooseSignInTarget(off, null)).toBe("host-fallback");
    // Packages installed, cache empty — the fresh-project state, and the one
    // that used to be the silent default.
    expect(
      chooseSignInTarget(
        off,
        usableDetection({ browsers: [], chromium_executable_exists: false }),
      ),
    ).toBe("host-fallback");
    // Playwright too old to bind: the pane cannot show it either.
    expect(chooseSignInTarget(off, usableDetection({ has_bind: false }))).toBe(
      "host-fallback",
    );
  });

  it("answers the host *fallback* when nothing is known at all", () => {
    // "Unknown" must not read as "bridged". A status call that never answered
    // is not evidence that something will carry the callback home.
    expect(chooseSignInTarget(null, null)).toBe("host-fallback");
  });

  it("separates a live bridge from the least-bad answer, though both lead with the host", () => {
    const off: AuthBridgeStatus = { enabled: false, active_ports: [], conflicts: [] };
    // The two states the old two-valued answer collapsed together. Folding them
    // back into one is what let the toast tell a user with the bridge disabled
    // that the bridge would carry their callback.
    expect(chooseSignInTarget(LIVE_BRIDGE, null)).not.toBe(
      chooseSignInTarget(off, null),
    );
  });
});

describe("TerminalView — the sign-in default follows the project", () => {
  const SIGN_IN =
    "https://claude.ai/oauth/authorize?code=true&client_id=abc&response_type=code";

  function relaySequence(url: string): number[] {
    return Array.from(
      new TextEncoder().encode(`\x1b]7777;open;${btoa(url)}\x07`),
    );
  }

  async function mountWithPrompt() {
    const view = mountSession("claude");
    await act(async () => {});
    const emit = ptyOutput.listeners.get("terminal-output-s1");
    if (!emit) throw new Error("no terminal-output listener registered");
    await act(async () => {
      emit({ payload: relaySequence(SIGN_IN) });
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });
    return view;
  }

  function primaryLabel(): string | null {
    return document.querySelector<HTMLElement>(
      '[data-url-toast-primary="true"]',
    )?.textContent ?? null;
  }

  function actionOrder(): (string | null)[] {
    return Array.from(document.querySelectorAll("button"))
      .map((b) => b.textContent)
      .filter((t) => t === "Open" || t === "In container");
  }

  it("leads with the host browser when the auth bridge is on", async () => {
    containerEnv.bridge = LIVE_BRIDGE;
    containerEnv.detection = usableDetection();
    await mountWithPrompt();
    expect(primaryLabel()).toBe("Open");
    // Both are still offered — this changes which leads, never which exist.
    expect(actionOrder()).toEqual(["Open", "In container"]);
  });

  it("leads with the container when the bridge is off and a browser is there", async () => {
    containerEnv.detection = usableDetection();
    await mountWithPrompt();
    expect(primaryLabel()).toBe("In container");
    expect(actionOrder()).toEqual(["In container", "Open"]);
  });

  it("leads with the host on a fresh project, where neither is set up", async () => {
    // Playwright is deliberately not baked into the image, so this is what a
    // project looks like until someone presses install — and pointing the
    // default at it failed on every platform, silently.
    await mountWithPrompt();
    expect(primaryLabel()).toBe("Open");
  });

  it("does not promise the auth bridge on a project that has it switched off", async () => {
    // The end-to-end version of the three-state answer: bridge off, no browser
    // inside. The host still leads, because it is the least bad of two answers
    // that can both fail — but the hint must not tell the user the bridge is
    // bringing their callback home, because there is no bridge. That hint is
    // what sent people to a host browser and a login that hung to its timeout.
    await mountWithPrompt();
    const hint = document.querySelector('[data-testid="url-toast-signin-hint"]');
    expect(hint?.textContent).toMatch(/nothing is set up/i);
    expect(hint?.textContent).not.toMatch(/what carries the callback/i);
  });

  it("does promise it when the bridge is actually live", async () => {
    containerEnv.bridge = LIVE_BRIDGE;
    await mountWithPrompt();
    const hint = document.querySelector('[data-testid="url-toast-signin-hint"]');
    expect(hint?.textContent).toMatch(/auth bridge/i);
    expect(hint?.textContent).not.toMatch(/nothing is set up/i);
  });
});

describe("TerminalView — a host open that fails says so", () => {
  const URL = "https://github.com/login/device?code=ABCD-EFGH";

  function relaySequence(url: string): number[] {
    return Array.from(
      new TextEncoder().encode(`\x1b]7777;open;${btoa(url)}\x07`),
    );
  }

  async function mountWithPrompt() {
    const view = mountSession("claude");
    await act(async () => {});
    const emit = ptyOutput.listeners.get("terminal-output-s1");
    if (!emit) throw new Error("no terminal-output listener registered");
    await act(async () => {
      emit({ payload: relaySequence(URL) });
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });
    return view;
  }

  function openButton(): HTMLElement {
    const el = Array.from(document.querySelectorAll("button")).find(
      (b) => b.textContent === "Open",
    );
    if (!el) throw new Error("Open button not found");
    return el as HTMLElement;
  }

  it("pushes a toast instead of a console line nobody reads", async () => {
    vi.mocked(openUrlExternal).mockRejectedValueOnce(new Error("no opener"));
    await mountWithPrompt();
    await act(async () => {
      fireEvent.click(openButton());
      await Promise.resolve();
    });
    const toasts = useAppState.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].kind).toBe("error");
    expect(toasts[0].detail).toContain("no opener");
  });

  it("keeps the prompt on screen, so the other route is still one click away", async () => {
    // Dismissing first is what this replaced: the toast vanished, nothing
    // opened, and the URL only existed in the container's transcript.
    vi.mocked(openUrlExternal).mockRejectedValueOnce(new Error("no opener"));
    await mountWithPrompt();
    await act(async () => {
      fireEvent.click(openButton());
      await Promise.resolve();
    });
    expect(document.querySelector(URL_TOAST_SELECTOR)).not.toBeNull();
  });

  it("dismisses the prompt once the handoff actually succeeded", async () => {
    await mountWithPrompt();
    await act(async () => {
      fireEvent.click(openButton());
      await Promise.resolve();
    });
    expect(openUrlExternal).toHaveBeenCalledWith(URL);
    expect(document.querySelector(URL_TOAST_SELECTOR)).toBeNull();
  });
});

describe("TerminalView — an open in flight must not blank a newer prompt", () => {
  // The window is real and is measured in hundreds of milliseconds, not in
  // microtasks: on Linux the opener sleeps `OPENER_GRACE` (400 ms, doubled when
  // `xdg-open` fails and `gio` is tried) before resolving. The container is free
  // to relay a second URL inside it — a `gh auth login` right after a
  // `claude login` is the ordinary way that happens — and the toast slot is
  // shared, so by the time the first open answers the slot may be holding a
  // prompt the user has never seen. Blanking it loses that URL for good: it
  // exists nowhere but the container's transcript.
  const URL_A = "https://github.com/login/device?code=AAAA-1111";
  const URL_B = "https://claude.ai/oauth/authorize?code=true&client_id=b";

  function relaySequence(url: string): number[] {
    return Array.from(
      new TextEncoder().encode(`\x1b]7777;open;${btoa(url)}\x07`),
    );
  }

  async function emitRelay(url: string) {
    const emit = ptyOutput.listeners.get("terminal-output-s1");
    if (!emit) throw new Error("no terminal-output listener registered");
    await act(async () => {
      emit({ payload: relaySequence(url) });
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  function openButton(): HTMLElement {
    const el = Array.from(document.querySelectorAll("button")).find(
      (b) => b.textContent === "Open",
    );
    if (!el) throw new Error("Open button not found");
    return el as HTMLElement;
  }

  function promptedUrl(): string | null {
    return (
      document
        .querySelector('[data-testid="url-toast-url"]')
        ?.getAttribute("title") ?? null
    );
  }

  /** An `openUrlExternal` that hangs until the test lets it finish. */
  function deferredOpen(): () => void {
    let finish: () => void = () => {};
    vi.mocked(openUrlExternal).mockReturnValueOnce(
      new Promise<void>((resolve) => {
        finish = () => resolve();
      }),
    );
    return () => finish();
  }

  it("keeps URL B's prompt when A's open resolves after B arrived", async () => {
    const finishOpen = deferredOpen();
    mountSession("claude");
    await act(async () => {});
    await emitRelay(URL_A);

    await act(async () => {
      fireEvent.click(openButton());
    });
    expect(openUrlExternal).toHaveBeenCalledWith(URL_A);

    // The container supersedes it while the opener is still inside its grace.
    await emitRelay(URL_B);
    expect(promptedUrl()).toBe(URL_B);

    await act(async () => {
      finishOpen();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(document.querySelector(URL_TOAST_SELECTOR)).not.toBeNull();
    expect(promptedUrl()).toBe(URL_B);
  });

  it("still dismisses when the slot is holding the prompt that was opened", async () => {
    // The other half of the guard: it must not turn "dismiss on success" into
    // "never dismiss". Same deferred open, nothing superseding it.
    const finishOpen = deferredOpen();
    mountSession("claude");
    await act(async () => {});
    await emitRelay(URL_A);

    await act(async () => {
      fireEvent.click(openButton());
    });
    expect(document.querySelector(URL_TOAST_SELECTOR)).not.toBeNull();

    await act(async () => {
      finishOpen();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(document.querySelector(URL_TOAST_SELECTOR)).toBeNull();
  });
});

describe("TerminalView — focus on request", () => {
  /** Mount, then deliberately give focus away, so what the assertions below
   *  observe is the *request* taking effect and never the focus `active`
   *  already grants on mount. That distinction is the whole point: the notes
   *  dock sends to a terminal whose tab is already active, where nothing
   *  changes and no `active` effect re-runs. */
  async function mountAndBlur() {
    const view = mountSession("claude");
    await act(async () => {});
    const elsewhere = document.createElement("button");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    expect(document.activeElement).toBe(elsewhere);
    return view;
  }

  it("focuses the terminal named by the request", async () => {
    const view = await mountAndBlur();

    await act(async () => {
      useAppState.getState().requestTerminalFocus("s1");
    });

    expect(document.activeElement).toBe(helperTextarea(view.container));
  });

  it("ignores a request meant for another session", async () => {
    const view = await mountAndBlur();
    const before = document.activeElement;

    await act(async () => {
      useAppState.getState().requestTerminalFocus("s2");
    });

    expect(document.activeElement).toBe(before);
    expect(document.activeElement).not.toBe(helperTextarea(view.container));
  });

  it("clears the request, so a second send focuses again", async () => {
    const view = await mountAndBlur();

    await act(async () => {
      useAppState.getState().requestTerminalFocus("s1");
    });
    expect(useAppState.getState().pendingTerminalFocus).toBeNull();

    const elsewhere = document.querySelector("button");
    (elsewhere as HTMLButtonElement).focus();

    await act(async () => {
      useAppState.getState().requestTerminalFocus("s1");
    });
    expect(document.activeElement).toBe(helperTextarea(view.container));
  });
});
describe("TerminalView — releasing a captured mouse", () => {
  /** Feed raw bytes to the terminal as if the container had printed them, and
   *  let xterm drain its write queue (it parses asynchronously). */
  async function emitBytes(text: string) {
    const emit = ptyOutput.listeners.get("terminal-output-s1");
    if (!emit) throw new Error("no terminal-output listener registered");
    await act(async () => {
      emit({ payload: Array.from(new TextEncoder().encode(text)) });
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  /** What the status bar would render from: the active terminal publishes the
   *  capture state, and the release action, into the store. The control itself
   *  lives in `StatusBar` — deliberately, so it never sits on top of the TUI
   *  that is asking for the mouse. */
  function captured(): boolean {
    return useAppState.getState().terminalMouseCaptured;
  }

  it("shows nothing while the container has not grabbed the mouse", async () => {
    mountSession("claude");
    await act(async () => {});

    expect(captured()).toBe(false);
  });

  it("surfaces a release control once the container turns mouse tracking on", async () => {
    // `?1003h` is any-event tracking: every mouse *move* over the terminal is
    // reported to the app. When the TUI that asked for it dies without
    // resetting the mode, xterm keeps routing moves to the PTY and drops text
    // selection — the freeze this control exists to break out of.
    mountSession("claude");
    await act(async () => {});

    await emitBytes("\x1b[?1003h\x1b[?1006h");

    expect(captured()).toBe(true);
  });

  it("clears the mode locally, without sending a byte to the container", async () => {
    // The reset is written into xterm's own parser, not onto the wire. The
    // program inside is usually gone; if it is not, it must not be told the
    // user pulled the mouse back, or a live TUI would just re-grab it.
    mountSession("claude");
    await act(async () => {});
    await emitBytes("\x1b[?1003h");
    terminalInput.mockClear();

    // Exactly what the status-bar button's onClick does.
    const release = useAppState.getState().releaseActiveMouse;
    await act(async () => {
      release();
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });

    // The published flag is bound to the live mode, so it going false *is* the
    // assertion that xterm's mouse tracking is back to "none".
    expect(captured()).toBe(false);
    expect(terminalInput).not.toHaveBeenCalled();
  });

  it("releases on Ctrl+Shift+X, for when the pointer itself is unusable", async () => {
    const { container } = mountSession("claude");
    await act(async () => {});
    await emitBytes("\x1b[?1002h");
    terminalInput.mockClear();

    await act(async () => {
      fireEvent.keyDown(helperTextarea(container), {
        key: "X",
        ctrlKey: true,
        shiftKey: true,
      });
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(captured()).toBe(false);
    // The chord must not also reach the container as input.
    expect(terminalInput).not.toHaveBeenCalled();
  });
});

describe("the hover hint names the key that actually works", () => {
  const platform = (value: string) =>
    Object.defineProperty(navigator, "platform", { value, configurable: true });
  const original = navigator.platform;
  afterEach(() => platform(original));

  // xterm gates this on its own `isMac`; if the hint and the gate disagree the
  // user is told to press a key that does nothing.
  it("says Option on a Mac, because that is xterm's force-selection modifier there", () => {
    platform("MacIntel");
    const host = document.createElement("div");
    createOsc8LinkHandler(() => host).hover?.(
      new MouseEvent("mousemove"),
      "https://example.com/x",
      { start: { x: 1, y: 1 }, end: { x: 1, y: 1 } },
    );
    expect(host.textContent).toContain("Option+click");
    expect(host.textContent).not.toContain("Shift+click");
  });

  it("says Shift everywhere else", () => {
    platform("Linux x86_64");
    const host = document.createElement("div");
    createOsc8LinkHandler(() => host).hover?.(
      new MouseEvent("mousemove"),
      "https://example.com/x",
      { start: { x: 1, y: 1 }, end: { x: 1, y: 1 } },
    );
    expect(host.textContent).toContain("Shift+click");
  });
});

describe("createOsc8LinkHandler — clicking a link Claude Code printed", () => {
  /**
   * The handler is exercised directly rather than through a rendered terminal.
   *
   * xterm decides *when* to call it from cell geometry, and jsdom gives every
   * element a zero-sized box — so a test driving the mouse over the pane would
   * be asserting that jsdom's layout engine exists, not that this app validates
   * what it opens. What xterm hands over is the OSC 8 parameter verbatim, which
   * is exactly what these arguments are.
   */
  const range = {
    start: { x: 1, y: 1 },
    end: { x: 80, y: 1 },
  } as unknown as Parameters<
    NonNullable<ReturnType<typeof createOsc8LinkHandler>["hover"]>
  >[2];

  let host: HTMLDivElement;
  let handler: ReturnType<typeof createOsc8LinkHandler>;

  beforeEach(() => {
    host = document.createElement("div");
    document.body.appendChild(host);
    handler = createOsc8LinkHandler(() => host);
  });

  function hoverCard(): HTMLElement | null {
    return host.querySelector<HTMLElement>(`.${OSC8_HOVER_CLASS}`);
  }

  it("refuses a target that fails validation, without reaching the opener", () => {
    // The visible text can be anything; the parameter is what gets opened, and
    // a container is free to put a scheme in it that the host must never hand
    // to an OS-level opener.
    handler.activate(new MouseEvent("click"), "javascript:alert(1)", range);
    handler.activate(new MouseEvent("click"), "file:///etc/passwd", range);
    handler.activate(
      new MouseEvent("click"),
      "https://claude.ai@evil.tld/authorize",
      range,
    );

    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("opens a valid target through the one sink", async () => {
    const url =
      "https://claude.ai/oauth/authorize?code=true&client_id=abc123&scope=user%3Ainference";
    await act(async () => {
      handler.activate(new MouseEvent("click"), url, range);
      await Promise.resolve();
    });

    expect(openUrlExternal).toHaveBeenCalledWith(url);
  });

  it("shows the real origin on hover, not the text on screen", () => {
    // The point of the affordance. OSC 8 decouples label from target: the row
    // can read `https://claude.ai` while the parameter points anywhere.
    handler.hover?.(
      new MouseEvent("mousemove"),
      "https://evil.example.com/claude.ai/oauth/authorize?code=true",
      range,
    );

    const card = hoverCard();
    expect(card).not.toBeNull();
    const origin = card!.querySelector('[data-testid="osc8-hover-origin"]');
    expect(origin?.textContent).toBe("https://evil.example.com");
    // Whole origin or nothing — a truncated one is the spoof this prevents.
    expect(origin?.textContent).not.toContain("…");
    expect(card!.textContent).not.toContain("https://claude.ai");

    handler.leave?.(new MouseEvent("mouseout"), "https://evil.example.com/", range);
    expect(hoverCard()).toBeNull();
  });

  it("says so on hover when the target would be refused", () => {
    handler.hover?.(new MouseEvent("mousemove"), "javascript:alert(1)", range);

    const card = hoverCard();
    expect(card).not.toBeNull();
    expect(card!.querySelector('[data-testid="osc8-hover-origin"]')).toBeNull();
    // Never echo the rejected target: it is untrusted text on its way to a DOM
    // node, and the only thing worth saying is that clicking does nothing.
    expect(card!.textContent).not.toContain("javascript:");
  });

  it("pushes the shared toast when the host opener fails", async () => {
    vi.mocked(openUrlExternal).mockRejectedValueOnce(new Error("no opener"));

    await act(async () => {
      handler.activate(new MouseEvent("click"), "https://example.com/x", range);
      await Promise.resolve();
    });

    const toasts = useAppState.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].kind).toBe("error");
    expect(toasts[0].detail).toContain("no opener");
    // Same card as every other dead-opener report in this view.
    expect(toasts[0].dedupeKey).toBe("host-open-failed");
  });
});
