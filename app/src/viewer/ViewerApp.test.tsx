import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import type { ViewerState } from "../lib/types";
import ViewerApp from "./ViewerApp";

const commands = vi.hoisted(() => ({
  viewerGetState: vi.fn(),
  viewerChooseFile: vi.fn(),
}));
vi.mock("../lib/tauri-commands", () => commands);
// The editor itself is covered by EditorPane.test; here only the routing matters.
vi.mock("./EditorPane", () => ({
  default: ({ state }: { state: ViewerState }) => (
    <p>editor for {state.state.kind === "resolved" ? state.state.container_path : "?"}</p>
  ),
}));

const base = { project_id: "p", project_name: "Demo", raw_path: "foo.ts", initial: { line: 3, col: null, end_line: null } };

describe("ViewerApp", () => {
  beforeEach(() => {
    commands.viewerGetState.mockReset();
    commands.viewerChooseFile.mockReset();
  });

  it("opens the editor for a resolved file", async () => {
    commands.viewerGetState.mockResolvedValue({ ...base, state: { kind: "resolved", container_path: "/workspace/a/foo.ts" } });
    render(<ViewerApp />);
    expect(await screen.findByText("editor for /workspace/a/foo.ts")).toBeInTheDocument();
  });

  it("lists every path it tried when the file is not found", async () => {
    commands.viewerGetState.mockResolvedValue({ ...base, state: { kind: "not_found", tried: ["/workspace/a/foo.ts", "/workspace/b/foo.ts"] } });
    render(<ViewerApp />);
    expect(await screen.findByText(/Could not find/)).toBeInTheDocument();
    expect(screen.getByText("/workspace/a/foo.ts")).toBeInTheDocument();
    expect(screen.getByText("/workspace/b/foo.ts")).toBeInTheDocument();
  });

  it("choosing a candidate asks the backend by index and opens the result", async () => {
    commands.viewerGetState.mockResolvedValue({ ...base, state: { kind: "choose", candidates: ["/workspace/a/foo.ts", "/workspace/b/foo.ts"] } });
    commands.viewerChooseFile.mockResolvedValue({ ...base, state: { kind: "resolved", container_path: "/workspace/b/foo.ts" } });
    render(<ViewerApp />);
    const second = await screen.findByRole("button", { name: "/workspace/b/foo.ts" });
    await act(async () => { fireEvent.click(second); });
    expect(commands.viewerChooseFile).toHaveBeenCalledWith(1);
    expect(await screen.findByText("editor for /workspace/b/foo.ts")).toBeInTheDocument();
  });

  it("shows a failure to load the state", async () => {
    commands.viewerGetState.mockRejectedValue("This window is not a file viewer.");
    render(<ViewerApp />);
    expect(await screen.findByText("This window is not a file viewer.")).toBeInTheDocument();
  });

  it("keeps the choice list when choosing fails, and says why", async () => {
    commands.viewerGetState.mockResolvedValue({ ...base, state: { kind: "choose", candidates: ["/workspace/a/foo.ts"] } });
    commands.viewerChooseFile.mockRejectedValue("That choice is no longer available.");
    render(<ViewerApp />);
    const only = await screen.findByRole("button", { name: "/workspace/a/foo.ts" });
    await act(async () => { fireEvent.click(only); });
    expect(await screen.findByText("That choice is no longer available.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "/workspace/a/foo.ts" })).toBeInTheDocument();
  });
});
