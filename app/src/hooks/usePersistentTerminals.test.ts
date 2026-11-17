import { describe, it, expect, vi, beforeEach } from "vitest";

const listWorkspaceTerminals = vi.fn();
const openTerminalSession = vi.fn();
vi.mock("../lib/tauri-commands", () => ({
  listWorkspaceTerminals: (id: string) => listWorkspaceTerminals(id),
  openTerminalSession: (...a: unknown[]) => openTerminalSession(...a),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { act, renderHook, waitFor } from "@testing-library/react";
import { useAppState } from "../store/appState";
import { resetPersistentTerminalSync, syncProjectTerminals, usePersistentTerminals } from "./usePersistentTerminals";
import { useTerminal } from "./useTerminal";
import type { Project, WorkspaceTerminal } from "../lib/types";

const aws = (container_id: string | null, status = "running") =>
  ({ id: "p1", name: "api", host_id: "h1", status, container_id }) as unknown as Project;
const t = (id: string, known: boolean): WorkspaceTerminal => ({
  session_id: id,
  kind: "claude",
  name: null,
  created_unix: 1,
  attached: false,
  known,
});

beforeEach(() => {
  resetPersistentTerminalSync();
  listWorkspaceTerminals.mockReset();
  useAppState.setState({ projects: [], sessions: [], tabOrder: [], activeTabKey: null, toasts: [] } as never);
});

describe("usePersistentTerminals", () => {
  it("reopens this app's running terminals once per connection, in the background", async () => {
    // Something else is the current tab: the reopened ones must not take focus.
    useAppState.getState().addSession({ id: "mine", projectId: "p2", projectName: "x", sessionType: "bash", sessionName: null });
    listWorkspaceTerminals.mockResolvedValue([t("a", true), t("elsewhere", false), t("b", true)]);
    useAppState.setState({ projects: [aws("http://127.0.0.1:5000/")] } as never);

    const { rerender } = renderHook(() => usePersistentTerminals());
    await waitFor(() => expect(useAppState.getState().sessions).toHaveLength(3));
    const s = useAppState.getState();
    expect(s.sessions.slice(1).map((x) => [x.id, x.pendingReattach, x.persistent])).toEqual([
      ["a", true, true],
      ["b", true, true],
    ]);
    expect(s.activeSessionId).toBe("mine");
    expect(s.toasts.some((x) => x.message.includes("Reopened 2 terminals"))).toBe(true);

    // The same connection again: nothing more is asked.
    rerender();
    act(() => useAppState.setState({ projects: [aws("http://127.0.0.1:5000/")] } as never));
    expect(listWorkspaceTerminals).toHaveBeenCalledTimes(1);

    // A new connection (re-attach on another port) lists again; open tabs
    // are not opened twice.
    act(() => useAppState.setState({ projects: [aws("http://127.0.0.1:6000/")] } as never));
    await waitFor(() => expect(listWorkspaceTerminals).toHaveBeenCalledTimes(2));
    expect(useAppState.getState().sessions).toHaveLength(3);
  });

  it("leaves Docker and stopped projects alone", async () => {
    useAppState.setState({
      projects: [
        { id: "d", name: "local", host_id: null, status: "running", container_id: "abc" } as unknown as Project,
        aws("http://127.0.0.1:5000/", "stopped"),
      ],
    } as never);
    renderHook(() => usePersistentTerminals());
    await new Promise((r) => setTimeout(r, 10));
    expect(listWorkspaceTerminals).not.toHaveBeenCalled();
  });
});

it("does not reopen a tab whose own open is still in flight", async () => {
  let finishOpen: (v: unknown) => void = () => {};
  openTerminalSession.mockImplementation(() => new Promise((r) => (finishOpen = r)));
  const { result } = renderHook(() => useTerminal());
  const opening = result.current.open("p1", "api");
  const id = openTerminalSession.mock.calls[0][1] as string;
  // The workspace already lists (and remembers) it.
  listWorkspaceTerminals.mockResolvedValue([t(id, true)]);
  expect(await syncProjectTerminals(aws("http://127.0.0.1:5000/"))).toBe(0);
  finishOpen({ persistent: true, notice: null });
  await act(async () => {
    await opening;
  });
  expect(useAppState.getState().sessions.filter((s) => s.id === id)).toHaveLength(1);
  expect(useAppState.getState().sessions[0].pendingReattach).toBeUndefined();
});
