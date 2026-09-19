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
 * What `TerminalView` actually handed the `Terminal` constructor, and the
 * instances it built.
 *
 * The real xterm is kept — these tests depend on its parser, its modes and its
 * DOM — and only the constructor is wrapped, because the wiring of
 * `linkHandler` is otherwise unobservable from outside: xterm decides when to
 * call it from cell geometry that jsdom has no layout for, so deleting the
 * `linkHandler:` line changed nothing any assertion could see.
 */
const xterm = vi.hoisted(() => ({
  options: null as Record<string, unknown> | null,
  instances: [] as unknown[],
}));

/**
 * The click handler `TerminalView` hands `WebLinksAddon`.
 *
 * Captured for the same reason the `Terminal` constructor is: xterm decides
 * when to call it from cell geometry jsdom has no layout for, so the only way
 * to ask "does the plain-text-URL path apply the same gate as the OSC 8 one?"
 * is to hold the function and call it.
 */
const webLinks = vi.hoisted(() => ({
  handler: null as null | ((event: MouseEvent, uri: string) => void),
}));

vi.mock("@xterm/xterm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@xterm/xterm")>();
  class SpyTerminal extends actual.Terminal {
    constructor(options?: ConstructorParameters<typeof actual.Terminal>[0]) {
      super(options);
      xterm.options = (options ?? null) as Record<string, unknown> | null;
      xterm.instances.push(this);
    }
  }
  return { ...actual, Terminal: SpyTerminal };
});

vi.mock("@xterm/addon-web-links", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@xterm/addon-web-links")>();
  type Args = ConstructorParameters<typeof actual.WebLinksAddon>;
  class SpyWebLinksAddon extends actual.WebLinksAddon {
    constructor(...args: Args) {
      super(...args);
      webLinks.handler = (args[0] ?? null) as typeof webLinks.handler;
    }
  }
  return { ...actual, WebLinksAddon: SpyWebLinksAddon };
});

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
  xterm.options = null;
  xterm.instances.length = 0;
  webLinks.handler = null;
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

  const hoverHint = (
    tracking: boolean,
    macOptionClickForcesSelection = true,
  ): string => {
    const host = document.createElement("div");
    createOsc8LinkHandler(() => host, () => ({
      mouseTracking: tracking,
      hasSelection: false,
      macOptionClickForcesSelection,
    })).hover?.(new MouseEvent("mousemove"), "https://example.com/x", {
      start: { x: 1, y: 1 },
      end: { x: 1, y: 1 },
    });
    return host.textContent ?? "";
  };

  // The hint and the gate read one predicate; these pin that they cannot
  // drift, because a hint naming a key the gate does not accept is the bug
  // that was already fixed once on this branch.
  it("says Option on a Mac, because that is xterm's force-selection modifier there", () => {
    platform("MacIntel");
    expect(hoverHint(true)).toContain("Option+click");
    expect(hoverHint(true)).not.toContain("Shift+click");
  });

  it("says Shift everywhere else", () => {
    platform("Linux x86_64");
    expect(hoverHint(true)).toContain("Shift+click");
  });

  // No program holds the mouse, so no modifier is needed — and naming one
  // would tell the user to press a key the gate ignores.
  it("names no modifier at all while nothing is tracking the mouse", () => {
    platform("Linux x86_64");
    const hint = hoverHint(false);
    expect(hint).toContain("Click to open");
    expect(hint).not.toContain("Shift+click");

    platform("MacIntel");
    expect(hoverHint(false)).not.toContain("Option+click");
  });

  // `macOptionClickForcesSelection` defaults to false in xterm and this view
  // sets it true, so the Mac branch is only live because of that line. If it
  // ever goes, Option stops being the force-selection modifier and the gate
  // can never pass while a program holds the mouse — so the card must not go
  // on naming a key that does nothing.
  it("does not promise Option+click when the option behind it is off", () => {
    platform("MacIntel");
    const hint = hoverHint(true, false);
    expect(hint).not.toContain("Option+click");
    expect(hint).not.toContain("Shift+click");
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
  /**
   * What the terminal answers about itself when the gate asks, per test.
   *
   * Mutable rather than fixed at construction because both of the first two
   * change *under* the handler: the container sets the mouse mode with a
   * DECSET, and the selection is whatever the gesture that ended in this
   * mouseup left behind.
   */
  let state: {
    mouseTracking: boolean;
    hasSelection: boolean;
    macOptionClickForcesSelection: boolean;
  };

  beforeEach(() => {
    host = document.createElement("div");
    document.body.appendChild(host);
    state = {
      mouseTracking: false,
      hasSelection: false,
      // What `TerminalView` sets on the real terminal.
      macOptionClickForcesSelection: true,
    };
    handler = createOsc8LinkHandler(() => host, () => state);
  });

  afterEach(() => host.remove());

  function hoverCard(): HTMLElement | null {
    return host.querySelector<HTMLElement>(`.${OSC8_HOVER_CLASS}`);
  }

  /** A real single click: one press, one release, `detail` 1. */
  const click = (init: MouseEventInit = {}) =>
    new MouseEvent("click", { button: 0, detail: 1, ...init });

  it("refuses a target that fails validation, without reaching the opener", () => {
    // The visible text can be anything; the parameter is what gets opened, and
    // a container is free to put a scheme in it that the host must never hand
    // to an OS-level opener.
    handler.activate(click(), "javascript:alert(1)", range);
    handler.activate(click(), "file:///etc/passwd", range);
    handler.activate(click(), "https://claude.ai@evil.tld/authorize", range);

    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("opens a valid target through the one sink", async () => {
    const url =
      "https://claude.ai/oauth/authorize?code=true&client_id=abc123&scope=user%3Ainference";
    await act(async () => {
      handler.activate(click(), url, range);
      await Promise.resolve();
    });

    expect(openUrlExternal).toHaveBeenCalledWith(url);
  });

  describe("the gate on activation", () => {
    const URL = "https://example.com/x";

    /**
     * The attack this gate exists for.
     *
     * xterm's mouse-reporting mousedown does *not* cancel anything —
     * `cancelEvents` defaults to false — and the Linkifier is a descendant of
     * the element those listeners are bound to, so the link layer sees every
     * click first and `_handleMouseUp` activates with no modifier, button or
     * mode check of its own. A TUI widget the user is meant to click can
     * therefore be wrapped in an OSC 8 pointing anywhere, and a plain click
     * opens the host browser on it while the mouse report still reaches the
     * program, so nothing looks wrong. The modifier is the only thing that
     * separates "I clicked the menu item" from "I asked to leave the app".
     */
    it("refuses a plain click while a program is tracking the mouse", () => {
      state.mouseTracking = true;

      handler.activate(click(), URL, range);

      expect(openUrlExternal).not.toHaveBeenCalled();
    });

    it("opens on the force-selection modifier while tracking", async () => {
      state.mouseTracking = true;

      await act(async () => {
        handler.activate(click({ shiftKey: true }), URL, range);
        await Promise.resolve();
      });

      expect(openUrlExternal).toHaveBeenCalledWith(URL);
    });

    it("opens on a plain click when nothing holds the mouse", async () => {
      // A normal shell. This is what `WebLinksAddon` does for the plain-text
      // URLs in the same buffer, and asking for a modifier here would read as
      // a broken link.
      state.mouseTracking = false;

      await act(async () => {
        handler.activate(click(), URL, range);
        await Promise.resolve();
      });

      expect(openUrlExternal).toHaveBeenCalledWith(URL);
    });

    it("ignores every button but the primary one", () => {
      // Right-click is the context menu this pane already binds; middle-click
      // is paste. Neither is a request to leave the app.
      state.mouseTracking = false;

      handler.activate(click({ button: 2 }), URL, range);
      handler.activate(click({ button: 1 }), URL, range);
      handler.activate(click({ button: 2, shiftKey: true }), URL, range);

      expect(openUrlExternal).not.toHaveBeenCalled();
    });

    /**
     * Selecting text is not asking to leave the app.
     *
     * `Linkifier._handleMouseUp` has no `detail` check, no drag threshold and
     * no timestamp — it activates whenever the mouseup lands on the same link
     * the mousedown did. `SelectionService` is bound on the *document* and the
     * Linkifier on `screenElement`, so the selection gesture and the link
     * activation both run, the link layer first. Every gesture below is one a
     * user makes to *copy* a string, and none of them may open a browser.
     */
    describe("a selection gesture is not a click", () => {
      it("refuses a double-click, which selects the word under it", () => {
        // xterm selects the word on the *mousedown* of the second click, so
        // by this mouseup the selection is already there.
        state.hasSelection = true;

        handler.activate(click({ detail: 2 }), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });

      it("refuses a triple-click, which selects the whole row", () => {
        state.hasSelection = true;

        handler.activate(click({ detail: 3 }), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });

      // The one the click count cannot see: a drag is a single press and a
      // single release, so `detail` is 1 throughout. Only the selection it
      // left behind distinguishes it from a click.
      it("refuses a drag that selected characters, at click count 1", () => {
        state.hasSelection = true;

        handler.activate(click(), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });

      /**
       * The worst version, and the reason the modifier alone is not a gate.
       *
       * While a program holds the mouse, Shift/Option+drag is the *only* way
       * to select text at all — so "the deliberate request to leave the app"
       * and "I am copying this line" are byte-identical gestures. A container
       * that wraps each of its output rows in an OSC 8 turns every legitimate
       * copy into a browser open.
       */
      it("refuses a force-selection drag while a program holds the mouse", () => {
        state.mouseTracking = true;
        state.hasSelection = true;

        handler.activate(click({ shiftKey: true }), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });

      // Belt to the selection check's braces: independent of whether xterm
      // managed to select anything (a double-click on trailing whitespace
      // selects nothing), a second click is not a first one.
      it("refuses a repeat click even when nothing ended up selected", () => {
        state.hasSelection = false;

        handler.activate(click({ detail: 2 }), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });
    });

    /**
     * The card and the gate must not disagree about what the user has to do.
     *
     * The hint is computed once, when the pointer arrives; the mode it was
     * computed from is the container's to change, and `?1002l` takes effect
     * synchronously with the write. So a card reading "Shift+click to open"
     * can be on screen while the live mode says a bare click is enough —
     * which is also the shape of the flicker attack in FINDING 2. The gate
     * therefore honours the *stricter* of what was promised and what is true
     * now: a modifier the card asked for is still required when the click
     * lands.
     */
    describe("what the card promised still binds when the click lands", () => {
      it("keeps demanding the modifier after the container drops tracking", () => {
        state.mouseTracking = true;
        handler.hover?.(new MouseEvent("mousemove"), URL, range);
        expect(host.textContent).toContain("+click to open");

        // `?1002l`, mid-hover.
        state.mouseTracking = false;
        handler.activate(click(), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });

      it("still opens on the modifier the card named", async () => {
        state.mouseTracking = true;
        handler.hover?.(new MouseEvent("mousemove"), URL, range);
        state.mouseTracking = false;

        await act(async () => {
          handler.activate(click({ shiftKey: true }), URL, range);
          await Promise.resolve();
        });

        expect(openUrlExternal).toHaveBeenCalledWith(URL);
      });

      it("does not hold a stale demand against the next link", async () => {
        state.mouseTracking = true;
        handler.hover?.(new MouseEvent("mousemove"), URL, range);
        handler.leave?.(new MouseEvent("mouseout"), URL, range);

        // A plain shell now, and a fresh card that says so.
        state.mouseTracking = false;
        handler.hover?.(new MouseEvent("mousemove"), URL, range);
        expect(host.textContent).toContain("Click to open");

        await act(async () => {
          handler.activate(click(), URL, range);
          await Promise.resolve();
        });

        expect(openUrlExternal).toHaveBeenCalledWith(URL);
      });
    });

    /**
     * FINDING 6: the modifier is xterm's, including the option it hangs on.
     *
     * xterm's rule is `isMac ? altKey && macOptionClickForcesSelection :
     * shiftKey`. Hardcoding `altKey` agrees with the app only for as long as
     * the app keeps setting that option, and nothing tells you when it stops.
     */
    describe("the Mac modifier follows the terminal's own option", () => {
      const platform = (value: string) =>
        Object.defineProperty(navigator, "platform", {
          value,
          configurable: true,
        });
      const original = navigator.platform;
      afterEach(() => platform(original));

      it("opens on Option+click while the option is on", async () => {
        platform("MacIntel");
        state.mouseTracking = true;
        state.macOptionClickForcesSelection = true;

        await act(async () => {
          handler.activate(click({ altKey: true }), URL, range);
          await Promise.resolve();
        });

        expect(openUrlExternal).toHaveBeenCalledWith(URL);
      });

      it("refuses Option+click when the terminal does not treat it as force-select", () => {
        platform("MacIntel");
        state.mouseTracking = true;
        state.macOptionClickForcesSelection = false;

        handler.activate(click({ altKey: true }), URL, range);

        expect(openUrlExternal).not.toHaveBeenCalled();
      });

      it("ignores the option off a Mac, where Shift is the modifier", async () => {
        platform("Linux x86_64");
        state.mouseTracking = true;
        state.macOptionClickForcesSelection = false;

        await act(async () => {
          handler.activate(click({ shiftKey: true }), URL, range);
          await Promise.resolve();
        });

        expect(openUrlExternal).toHaveBeenCalledWith(URL);
      });
    });
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
    expect(card!.textContent).not.toContain("https://claude.ai");

    handler.leave?.(new MouseEvent("mouseout"), "https://evil.example.com/", range);
    expect(hoverCard()).toBeNull();
  });

  it("keeps a very long origin whole, and gives way in the remainder instead", () => {
    // The attacker picks the origin's length. `https://claude.ai.<300 a's>
    // .evil.tld/` parses, passes every `sanitizeRelayUrl` rule, and under a
    // non-shrinking flex item runs off the right edge of the pane — which
    // hides the registrable domain just as effectively as an ellipsis would.
    const origin = `https://claude.ai.${"a".repeat(300)}.${"b".repeat(200)}.evil.tld`;
    handler.hover?.(new MouseEvent("mousemove"), `${origin}/oauth?code=1`, range);

    const originEl = hoverCard()!.querySelector<HTMLElement>(
      '[data-testid="osc8-hover-origin"]',
    )!;
    // Whole origin or nothing: every character is in the DOM...
    expect(originEl.textContent).toBe(origin);
    // ...and it is allowed to wrap rather than be clipped or pushed off-pane.
    expect(originEl.style.flexShrink).not.toBe("0");
    expect(originEl.style.whiteSpace).not.toBe("nowrap");
    expect(originEl.style.overflowWrap).toBe("anywhere");

    // The truncatable half is the remainder, and only the remainder.
    const restEl = hoverCard()!.querySelector<HTMLElement>(
      '[data-testid="osc8-hover-rest"]',
    )!;
    expect(restEl.style.textOverflow).toBe("ellipsis");
    expect(restEl.style.whiteSpace).toBe("nowrap");
  });

  it("cannot take the pointer away from the link that summoned it", () => {
    // The card is appended to `Terminal.element`, a *sibling* of the
    // `screenElement` the Linkifier listens on, so `xterm-hover` buys nothing
    // here: a card under the pointer means `mouseleave` on screenElement, the
    // card is torn down, and the mouseup that would activate the link lands on
    // the card instead of the terminal.
    handler.hover?.(new MouseEvent("mousemove"), "https://example.com/x", range);

    expect(hoverCard()!.style.pointerEvents).toBe("none");
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

  it("does not call a refused web address something other than a web address", () => {
    // `https://claude.ai@evil.tld/` is a perfectly good URL; it is refused
    // because the userinfo makes the visible host a lie. Telling the user it
    // "is not a web address" is false, and a false explanation teaches them to
    // distrust the card.
    handler.hover?.(
      new MouseEvent("mousemove"),
      "https://claude.ai@evil.tld/authorize",
      range,
    );

    expect(hoverCard()!.textContent).not.toContain("not a web address");
  });

  it("drops a stale card when the pane is no longer on screen", () => {
    // `leave` only ever arrives from the Linkifier's `_clearCurrentLink`, and
    // switching tabs from the keyboard moves no pointer: without this, the
    // card is still sitting there when the user comes back.
    handler.hover?.(new MouseEvent("mousemove"), "https://example.com/x", range);
    expect(hoverCard()).not.toBeNull();

    handler.dismiss();

    expect(hoverCard()).toBeNull();
  });

  it("pushes the shared toast when the host opener fails", async () => {
    vi.mocked(openUrlExternal).mockRejectedValueOnce(new Error("no opener"));

    await act(async () => {
      handler.activate(click(), "https://example.com/x", range);
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

describe("the link handler is wired into the terminal, and reads its live mode", () => {
  const range = {
    start: { x: 1, y: 1 },
    end: { x: 80, y: 1 },
  } as unknown as Parameters<
    NonNullable<ReturnType<typeof createOsc8LinkHandler>["hover"]>
  >[2];

  /** What the mounted view passed as `linkHandler`. */
  function wiredHandler() {
    const handler = xterm.options?.linkHandler as
      | ReturnType<typeof createOsc8LinkHandler>
      | undefined;
    if (!handler) throw new Error("no linkHandler was passed to Terminal");
    return handler;
  }

  /** Feed the terminal a DECSET the way the container would. */
  async function write(data: string) {
    const term = xterm.instances.at(-1) as { write(d: string, cb: () => void): void };
    await act(
      () => new Promise<void>((resolve) => term.write(data, resolve)),
    );
  }

  it("passes one at all — without it OSC 8 links are inert", () => {
    mountSession("claude");

    const handler = wiredHandler();
    expect(typeof handler.activate).toBe("function");
    expect(typeof handler.hover).toBe("function");
  });

  // The gate has to ask the terminal, not a boolean captured at construction:
  // the mode changes whenever the container prints a DECSET, which is several
  // times a second in Claude Code.
  it("refuses a plain click once the container turns mouse tracking on", async () => {
    mountSession("claude");
    await write("\x1b[?1002h");

    wiredHandler().activate(
      new MouseEvent("click", { button: 0 }),
      "https://example.com/x",
      range,
    );

    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("opens again once the container gives the mouse back", async () => {
    mountSession("claude");
    await write("\x1b[?1002h");
    await write("\x1b[?1002l");

    await act(async () => {
      wiredHandler().activate(
        new MouseEvent("click", { button: 0 }),
        "https://example.com/x",
        range,
      );
      await Promise.resolve();
    });

    expect(openUrlExternal).toHaveBeenCalledWith("https://example.com/x");
  });
});

/**
 * FINDING 4: the sibling path opens the same browser.
 *
 * `WebLinksAddon` matches rendered text and activates through the same
 * `Linkifier._handleMouseUp`, with the same absence of any check. It also
 * picks up links the OSC 8 handler never sees: `OscLinkProvider` drops a
 * non-http(s) hyperlink target before `linkHandler` is reached, which leaves
 * the addon free to match the *label* — so an OSC 8 with a `javascript:`
 * target and an `https://evil.tld/x` label arrives here and nowhere else.
 * Both routes end at `openUrlExternal`, so both ask the same question first.
 */
describe("the plain-text URL path is gated the same way", () => {
  function webLinksHandler() {
    if (!webLinks.handler) throw new Error("no handler was passed to WebLinksAddon");
    return webLinks.handler;
  }

  function term() {
    return xterm.instances.at(-1) as unknown as {
      write(d: string, cb: () => void): void;
      select(column: number, row: number, length: number): void;
    };
  }

  async function write(data: string) {
    await act(() => new Promise<void>((resolve) => term().write(data, resolve)));
  }

  const click = (init: MouseEventInit = {}) =>
    new MouseEvent("click", { button: 0, detail: 1, ...init });
  const URL = "https://example.com/x";

  it("opens on a plain click in an ordinary shell", async () => {
    mountSession("bash");

    await act(async () => {
      webLinksHandler()(click(), URL);
      await Promise.resolve();
    });

    expect(openUrlExternal).toHaveBeenCalledWith(URL);
  });

  it("refuses a plain click while a program holds the mouse", async () => {
    mountSession("claude");
    await write("\x1b[?1002h");

    webLinksHandler()(click(), URL);

    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("opens on the force-selection modifier while tracking", async () => {
    mountSession("claude");
    await write("\x1b[?1002h");

    await act(async () => {
      webLinksHandler()(click({ shiftKey: true }), URL);
      await Promise.resolve();
    });

    expect(openUrlExternal).toHaveBeenCalledWith(URL);
  });

  it("refuses the mouseup that ended a selection", async () => {
    mountSession("bash");
    await write("https://example.com/x");
    await act(async () => {
      term().select(0, 0, 5);
    });

    webLinksHandler()(click(), URL);

    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("refuses a repeat click", async () => {
    mountSession("bash");

    webLinksHandler()(click({ detail: 2 }), URL);

    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("still refuses a target that fails validation", async () => {
    mountSession("bash");

    webLinksHandler()(click(), "https://claude.ai@evil.tld/authorize");

    expect(openUrlExternal).not.toHaveBeenCalled();
  });
});
