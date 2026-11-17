import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { Project, WorkspaceTerminal } from "../../../lib/types";

const listWorkspaceTerminals = vi.fn();
const endWorkspaceTerminal = vi.fn(async () => {});
const closeTerminalSession = vi.fn(async () => {});
vi.mock("../../../lib/tauri-commands", () => ({
  listClaudeSessions: vi.fn(async () => []),
  listWorkspaceTerminals: (id: string) => listWorkspaceTerminals(id),
  endWorkspaceTerminal: (p: string, s: string) => endWorkspaceTerminal(p, s),
  closeTerminalSession: (s: string) => closeTerminalSession(s),
  updateProject: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import SessionsTab from "./SessionsTab";
import { useAppState } from "../../../store/appState";

const actions = { resumeClaudeSession: vi.fn() } as never;
const aws = { id: "p1", name: "api", host_id: "h1", status: "running", container_id: "http://x/" } as unknown as Project;
const t = (id: string, over: Partial<WorkspaceTerminal> = {}): WorkspaceTerminal => ({
  session_id: id,
  kind: "claude",
  name: null,
  created_unix: Math.floor(Date.now() / 1000) - 600,
  attached: false,
  known: true,
  ...over,
});

beforeEach(() => {
  listWorkspaceTerminals.mockReset();
  endWorkspaceTerminal.mockClear();
  closeTerminalSession.mockClear();
  useAppState.setState({ projects: [aws], sessions: [], tabOrder: [], activeTabKey: null } as never);
});

describe("SessionsTab — terminals in an AWS workspace", () => {
  it("lists them with their state and reattaches one in a tab", async () => {
    listWorkspaceTerminals.mockResolvedValue([
      t("open-1", { name: "main" }),
      t("det-2", { kind: "bash" }),
      t("web-3", { known: false, attached: true, name: "web terminal" }),
    ]);
    useAppState.getState().addSession({ id: "open-1", projectId: "p1", projectName: "api", sessionType: "claude", sessionName: "main", persistent: true });
    render(<SessionsTab project={aws} actions={actions} />);

    const list = await screen.findByRole("list", { name: "Terminals in the workspace" });
    expect(list.textContent).toContain("main");
    expect(list.textContent).toContain("Open here");
    expect(list.textContent).toContain("Shell · Detached");
    expect(list.textContent).toContain("Attached elsewhere");
    expect(list.textContent).toContain("opened elsewhere");
    // Open tabs have nothing to reattach.
    expect(screen.getAllByRole("button", { name: "Reattach" })).toHaveLength(2);

    fireEvent.click(screen.getAllByRole("button", { name: "Reattach" })[0]);
    const tab = useAppState.getState().sessions.find((s) => s.id === "det-2");
    expect(tab).toMatchObject({ sessionType: "bash", persistent: true, pendingReattach: true, projectId: "p1" });
    expect(useAppState.getState().activeSessionId).toBe("det-2");
  });

  it("ends a detached terminal in the workspace and an open one by closing its tab, after a confirm", async () => {
    listWorkspaceTerminals.mockResolvedValue([t("open-1"), t("det-2")]);
    useAppState.getState().addSession({ id: "open-1", projectId: "p1", projectName: "api", sessionType: "claude", sessionName: null, persistent: true });
    render(<SessionsTab project={aws} actions={actions} />);
    await screen.findByRole("list", { name: "Terminals in the workspace" });

    fireEvent.click(screen.getAllByRole("button", { name: "End" })[1]);
    expect(endWorkspaceTerminal).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm end" }));
    await waitFor(() => expect(endWorkspaceTerminal).toHaveBeenCalledWith("p1", "det-2"));

    fireEvent.click(screen.getAllByRole("button", { name: "End" })[0]);
    fireEvent.click(screen.getByRole("button", { name: "Confirm end" }));
    await waitFor(() => expect(closeTerminalSession).toHaveBeenCalledWith("open-1"));
    expect(useAppState.getState().sessions.find((s) => s.id === "open-1")).toBeUndefined();
  });

  it("is not shown for a Docker project", async () => {
    const docker = { ...aws, host_id: null } as unknown as Project;
    render(<SessionsTab project={docker} actions={actions} />);
    await screen.findByText(/No sessions recorded yet/);
    expect(screen.queryByText("Terminals in the workspace")).toBeNull();
    expect(listWorkspaceTerminals).not.toHaveBeenCalled();
  });
});
