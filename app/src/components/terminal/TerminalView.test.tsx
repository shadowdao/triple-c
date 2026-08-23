import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/react";
import TerminalView, { supersedes } from "./TerminalView";
import { useAppState } from "../../store/appState";

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
  listen: vi.fn(async () => () => {}),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }),
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
