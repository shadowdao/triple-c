import { describe, it, expect, vi, beforeEach } from "vitest";

// The queue lives at module scope in useTerminal, so the command layer is
// mocked and the hook's `sendInput` is exercised through `renderHook`.
const terminalInput = vi.fn<(sessionId: string, data: number[]) => Promise<void>>();

vi.mock("../lib/tauri-commands", () => ({
  terminalInput: (sessionId: string, data: number[]) => terminalInput(sessionId, data),
  openTerminalSession: vi.fn(),
  closeTerminalSession: vi.fn(),
  terminalResize: vi.fn(),
  pasteImageToTerminal: vi.fn(),
  updateProject: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { renderHook } from "@testing-library/react";
import { useTerminal } from "./useTerminal";

const decode = (bytes: number[]) => new TextDecoder().decode(new Uint8Array(bytes));

describe("useTerminal input ordering", () => {
  beforeEach(() => {
    terminalInput.mockReset();
  });

  it("preserves order even when the underlying invokes resolve out of order", async () => {
    // Make the *first* call the slowest, which is exactly the race that put a
    // backspace behind the characters typed after it.
    const resolvers: Array<() => void> = [];
    terminalInput.mockImplementation(
      () => new Promise<void>((resolve) => resolvers.push(resolve)),
    );

    const { result } = renderHook(() => useTerminal());

    const first = result.current.sendInput("s1", "\x7f"); // backspace
    const rest = ["a", "b", "c"].map((ch) => result.current.sendInput("s1", ch));

    // Only one write may be in flight at a time.
    expect(terminalInput).toHaveBeenCalledTimes(1);
    expect(decode(terminalInput.mock.calls[0][1])).toBe("\x7f");

    resolvers.shift()!();
    await first;

    // The three queued keystrokes coalesce into one ordered write.
    expect(terminalInput).toHaveBeenCalledTimes(2);
    expect(decode(terminalInput.mock.calls[1][1])).toBe("abc");

    resolvers.shift()!();
    await Promise.all(rest);

    const sent = terminalInput.mock.calls.map((c) => decode(c[1])).join("");
    expect(sent).toBe("\x7fabc");
  });

  it("settles each caller's promise and does not drop later writes on failure", async () => {
    terminalInput.mockRejectedValueOnce(new Error("boom")).mockResolvedValue(undefined);

    const { result } = renderHook(() => useTerminal());

    await expect(result.current.sendInput("s2", "x")).rejects.toThrow("boom");
    await expect(result.current.sendInput("s2", "y")).resolves.toBeUndefined();

    expect(decode(terminalInput.mock.calls[1][1])).toBe("y");
  });

  it("keeps separate sessions independent", async () => {
    terminalInput.mockResolvedValue(undefined);
    const { result } = renderHook(() => useTerminal());

    await Promise.all([
      result.current.sendInput("a", "1"),
      result.current.sendInput("b", "2"),
    ]);

    const bySession = terminalInput.mock.calls.map((c) => [c[0], decode(c[1])]);
    expect(bySession).toContainEqual(["a", "1"]);
    expect(bySession).toContainEqual(["b", "2"]);
  });
});
