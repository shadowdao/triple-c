import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import SendToAgentButton from "./SendToAgentButton";
import type { Project, TerminalSession } from "../../lib/types";

const sendInput = vi.fn(async () => {});
let sessions: TerminalSession[] = [];

vi.mock("../../hooks/useTerminal", () => ({
  useTerminal: () => ({ sessions, sendInput }),
}));

const setActiveTabKey = vi.fn();
const pushToast = vi.fn();
let projects: Project[] = [];

vi.mock("../../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) =>
      selector({ projects, setActiveTabKey, pushToast }),
    { getState: () => ({ projects, setActiveTabKey, pushToast }) },
  ),
  terminalTabKey: (id: string) => `term:${id}`,
}));

const session = (over: Partial<TerminalSession> = {}): TerminalSession => ({
  id: "s1",
  projectId: "p1",
  projectName: "api",
  sessionType: "claude",
  sessionName: null,
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  sessions = [];
  projects = [{ id: "p1", name: "api", renamed_session_names: {} } as unknown as Project];
});

describe("SendToAgentButton", () => {
  it("is disabled when the project has no running session", () => {
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });

  it("is disabled when the only session belongs to another project", () => {
    sessions = [session({ projectId: "other" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });

  it("is disabled when the only session is a bash tab", () => {
    // `bash -l`'s readline has no binding for ESC+CR and just bells, so a
    // shell is never a target.
    sessions = [session({ sessionType: "bash" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });

  it("sends straight to the one session, with newlines converted and no terminator", async () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body={"one\ntwo"} />);

    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));

    await waitFor(() => expect(sendInput).toHaveBeenCalledWith("s1", "one\x1b\rtwo"));
    expect(sendInput.mock.calls[0][1].endsWith("\r")).toBe(false);
  });

  it("focuses the terminal it sent to, so the user watches it land", async () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body="hi" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    await waitFor(() => expect(setActiveTabKey).toHaveBeenCalledWith("term:s1"));
  });

  it("offers a menu of display names when several sessions are open", async () => {
    sessions = [session(), session({ id: "s2", sessionName: "review" })];
    projects = [
      { id: "p1", name: "api", renamed_session_names: { s1: "release" } } as unknown as Project,
    ];
    render(<SendToAgentButton projectId="p1" body="hi" />);

    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    expect(sendInput).not.toHaveBeenCalled();

    fireEvent.click(await screen.findByRole("menuitem", { name: "api: release" }));
    await waitFor(() => expect(sendInput).toHaveBeenCalledWith("s1", "hi"));
  });

  it("reports a failed send rather than looking like it worked", async () => {
    sessions = [session()];
    sendInput.mockRejectedValueOnce(new Error("session closed"));
    render(<SendToAgentButton projectId="p1" body="hi" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
  });

  it("does nothing for an empty note", () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body="   " />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });
});
