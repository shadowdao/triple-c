import { describe, it, expect, vi, beforeEach } from "vitest";

const openTerminalSession = vi.fn();
const closeTerminalSession = vi.fn();
vi.mock("../lib/tauri-commands", () => ({
  openTerminalSession: (...a: unknown[]) => openTerminalSession(...a),
  closeTerminalSession: (id: string) => closeTerminalSession(id),
  terminalInput: vi.fn(),
  terminalResize: vi.fn(),
  pasteImageToTerminal: vi.fn(),
  updateProject: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { renderHook } from "@testing-library/react";
import { useTerminal } from "./useTerminal";
import { useAppState } from "../store/appState";

beforeEach(() => {
  openTerminalSession.mockReset();
  closeTerminalSession.mockReset();
  useAppState.setState({ projects: [], sessions: [], tabOrder: [], activeTabKey: null, toasts: [] } as never);
});

describe("useTerminal and persistent AWS terminals", () => {
  it("marks a tab persistent when the workspace keeps it", async () => {
    openTerminalSession.mockResolvedValue({ persistent: true, notice: null });
    const { result } = renderHook(() => useTerminal());
    const id = await result.current.open("p1", "api", "bash");
    expect(useAppState.getState().sessions.find((s) => s.id === id)?.persistent).toBe(true);
    expect(useAppState.getState().toasts).toHaveLength(0);
  });

  it("says once that the image has no tmux", async () => {
    openTerminalSession.mockResolvedValue({ persistent: false, notice: "This workspace image has no tmux, so …" });
    const { result } = renderHook(() => useTerminal());
    await result.current.open("p1", "api");
    await result.current.open("p1", "api");
    const toasts = useAppState.getState().toasts.filter((t) => t.message.includes("no tmux"));
    expect(toasts).toHaveLength(1);
    expect(useAppState.getState().sessions.every((s) => !s.persistent)).toBe(true);
  });

  it("closing a tab is not mistaken for a lost connection", async () => {
    openTerminalSession.mockResolvedValue({ persistent: true, notice: null });
    let persistentWhenClosed: boolean | undefined;
    closeTerminalSession.mockImplementation(async (id: string) => {
      persistentWhenClosed = useAppState.getState().sessions.find((s) => s.id === id)?.persistent;
    });
    const { result } = renderHook(() => useTerminal());
    const id = await result.current.open("p1", "api");
    await result.current.close(id);
    expect(persistentWhenClosed).toBe(false);
    expect(useAppState.getState().sessions).toHaveLength(0);
  });
});
