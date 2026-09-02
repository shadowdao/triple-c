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
const requestTerminalFocus = vi.fn();
const pushToast = vi.fn();
let projects: Project[] = [];

vi.mock("../../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) =>
      selector({ projects, setActiveTabKey, requestTerminalFocus, pushToast }),
    {
      getState: () => ({
        projects,
        setActiveTabKey,
        requestTerminalFocus,
        pushToast,
      }),
    },
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
  // Unavailable, not `disabled`: the reason a note cannot be sent is the whole
  // content of these states, and native `disabled` announces it to nobody.
  it("says why it cannot send when the project has no running session", () => {
    render(<SendToAgentButton projectId="p1" body="hello" />);
    const button = screen.getByRole("button", { name: /send to agent/i });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAccessibleDescription(
      "No running Claude session for this project",
    );
  });

  it("says why it cannot send an empty note", () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body="   " />);
    expect(
      screen.getByRole("button", { name: /send to agent/i }),
    ).toHaveAccessibleDescription("Nothing to send — this note is empty");
  });

  it("is unavailable when the only session belongs to another project", () => {
    sessions = [session({ projectId: "other" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(
      screen.getByRole("button", { name: /send to agent/i }),
    ).toHaveAttribute("aria-disabled", "true");
  });

  it("is unavailable when the only session is a bash tab", () => {
    // `bash -l`'s readline has no binding for ESC+CR and just bells, so a
    // shell is never a target.
    sessions = [session({ sessionType: "bash" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(
      screen.getByRole("button", { name: /send to agent/i }),
    ).toHaveAttribute("aria-disabled", "true");
  });

  // `aria-disabled` is advisory — it blocks nothing on its own. Without the
  // guard this swap would turn a greyed-out button into a live one.
  it("sends nothing when activated while unavailable", () => {
    render(<SendToAgentButton projectId="p1" body="hello" />);
    const button = screen.getByRole("button", { name: /send to agent/i });

    fireEvent.click(button);
    fireEvent.keyDown(button, { key: "Enter" });
    fireEvent.keyDown(button, { key: " " });

    expect(sendInput).not.toHaveBeenCalled();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
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
    const button = screen.getByRole("button", { name: /send to agent/i });
    expect(button).toHaveAttribute("aria-disabled", "true");

    fireEvent.click(button);
    fireEvent.keyDown(button, { key: "Enter" });
    expect(sendInput).not.toHaveBeenCalled();
  });

  it("opens the session menu upward when it sits at the foot of the dock", async () => {
    sessions = [session({ id: "s1" }), session({ id: "s2" })];
    render(<SendToAgentButton projectId="p1" body="hello" dropUp />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));

    // Anchored to the button's top edge, not below it: the dock clips its own
    // overflow, so a downward menu at the bottom edge is invisible.
    await waitFor(() => expect(screen.getByRole("menu")).toHaveClass("bottom-full"));
  });

  // Switching to the tab is not enough. When the dock is open beside the
  // terminal it sends to, that terminal is already the active tab, so
  // `setActiveTabKey` changes nothing and no effect re-runs — leaving focus on
  // this button, one click short of the Enter the user came to press.
  it("hands focus to the terminal so the next keystroke is Enter", async () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));

    await waitFor(() => expect(requestTerminalFocus).toHaveBeenCalledWith("s1"));
  });

  it("leaves focus alone when the send failed", async () => {
    sessions = [session()];
    sendInput.mockRejectedValueOnce(new Error("pty gone"));
    render(<SendToAgentButton projectId="p1" body="hello" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));

    await waitFor(() => expect(pushToast).toHaveBeenCalled());
    expect(requestTerminalFocus).not.toHaveBeenCalled();
  });

  it("focuses the session picked from the menu, not the first one", async () => {
    sessions = [session(), session({ id: "s2", sessionName: "review" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "review" }));

    await waitFor(() => expect(requestTerminalFocus).toHaveBeenCalledWith("s2"));
  });
});