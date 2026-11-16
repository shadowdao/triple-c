import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen } from "@testing-library/react";
import type { Project, WorkspaceStatus } from "../../../lib/types";

const awsWorkspaceStatus = vi.fn<(id: string) => Promise<WorkspaceStatus | null>>();
vi.mock("../../../lib/tauri-commands", () => ({
  awsWorkspaceStatus: (id: string) => awsWorkspaceStatus(id),
}));

import AwsWorkspaceStatusCard, { STATUS_POLL_MS } from "./AwsWorkspaceStatusCard";

const project = (over: Partial<Project> = {}): Project =>
  ({
    id: "p1",
    name: "demo",
    status: "running",
    host_id: "conn-1",
    container_id: "http://127.0.0.1:1",
    aws: { repos: [{}, {}, {}] },
    ...over,
  }) as unknown as Project;

const CLONING: WorkspaceStatus = {
  clone: {
    done: false,
    repos: [
      { folder: "api", state: "cloned" },
      { folder: "web", state: "cloning" },
    ],
  },
  setup: { state: "running", code: null, tail: [] },
};

const FAILED: WorkspaceStatus = {
  clone: {
    done: true,
    repos: [
      { folder: "api", state: "cloned" },
      { folder: "web", state: "failed" },
    ],
  },
  setup: { state: "failed", code: 2, tail: ["Setting up jq", "E: Unable to locate package nope"] },
};

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("AwsWorkspaceStatusCard", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    awsWorkspaceStatus.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows a workspace still cloning, then a failed clone and setup", async () => {
    awsWorkspaceStatus.mockResolvedValueOnce(CLONING).mockResolvedValue(FAILED);
    render(<AwsWorkspaceStatusCard project={project()} />);
    await flush();
    expect(screen.getByText("Still cloning repos (1 of 3 done)")).toBeTruthy();
    expect(screen.getByText("cloning…")).toBeTruthy();
    expect(screen.getByText("Setup script waits for the clone")).toBeTruthy();

    await act(async () => {
      vi.advanceTimersByTime(STATUS_POLL_MS);
    });
    await flush();
    expect(awsWorkspaceStatus).toHaveBeenCalledTimes(2);
    expect(screen.getByText("Clone failed for web")).toBeTruthy();
    expect(screen.getByText("Setup script failed (exit 2)")).toBeTruthy();
    expect(screen.getByText(/E: Unable to locate package nope/)).toBeTruthy();
  });

  it("renders nothing and does not poll for a stopped project", async () => {
    awsWorkspaceStatus.mockResolvedValue(FAILED);
    const { container } = render(<AwsWorkspaceStatusCard project={project({ status: "stopped" })} />);
    await flush();
    expect(awsWorkspaceStatus).not.toHaveBeenCalled();
    expect(container.innerHTML).toBe("");
  });

  it("renders nothing when there is nothing to say", async () => {
    awsWorkspaceStatus.mockResolvedValue({
      clone: { done: true, repos: [] },
      setup: { state: "none", code: null, tail: [] },
    });
    const { container } = render(<AwsWorkspaceStatusCard project={project()} />);
    await flush();
    expect(container.innerHTML).toBe("");
  });

  it("pauses while the window is hidden", async () => {
    awsWorkspaceStatus.mockResolvedValue(CLONING);
    render(<AwsWorkspaceStatusCard project={project()} />);
    await flush();
    expect(awsWorkspaceStatus).toHaveBeenCalledTimes(1);
    const state = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    await act(async () => {
      vi.advanceTimersByTime(STATUS_POLL_MS * 3);
    });
    await flush();
    expect(awsWorkspaceStatus).toHaveBeenCalledTimes(1);
    state.mockReturnValue("visible");
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await flush();
    expect(awsWorkspaceStatus).toHaveBeenCalledTimes(2);
    state.mockRestore();
  });
});
