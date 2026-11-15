import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useProjectActions } from "./useProjectActions";
import { useAppState } from "../store/appState";
import { awsProject, localProject } from "../test/awsFixtures";
import type { UnsavedWork } from "../lib/types";

const start = vi.fn();
const stop = vi.fn();
const checkUnsavedWork = vi.fn();
const awsTaskLog = vi.fn();

vi.mock("./useProjects", () => ({
  useProjects: () => ({ start, stop, rebuild: vi.fn() }),
}));
vi.mock("./useTerminal", () => ({
  useTerminal: () => ({ open: vi.fn(), sendInput: vi.fn() }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));
vi.mock("../lib/tauri-commands", () => ({
  checkUnsavedWork: (id: string) => checkUnsavedWork(id),
  awsTaskLog: (id: string, n: number) => awsTaskLog(id, n),
  downloadContainerBackup: vi.fn(),
}));

const clean: UnsavedWork = { checked: true, repos: [], error: null };
const dirty: UnsavedWork = {
  checked: true,
  repos: [{ folder: "api", branch: "main", dirty: true, unpushed: 0 }],
  error: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  useAppState.setState({ toasts: [] } as never);
});

const toasts = () => (useAppState.getState() as unknown as { toasts: { message: string; detail?: string }[] }).toasts;

describe("useProjectActions stop guard", () => {
  it("stops an AWS workspace with nothing unsaved straight away", async () => {
    checkUnsavedWork.mockResolvedValue(clean);
    const { result } = renderHook(() => useProjectActions(awsProject({ status: "running" })));
    await act(async () => {
      await result.current.handleStop();
    });
    expect(stop).toHaveBeenCalledWith("p1");
    expect(result.current.stopGuard).toBeNull();
  });

  it("asks first when there is unsaved work, then stops with the choice", async () => {
    checkUnsavedWork.mockResolvedValue(dirty);
    const { result } = renderHook(() => useProjectActions(awsProject({ status: "running" })));
    await act(async () => {
      await result.current.handleStop();
    });
    expect(stop).not.toHaveBeenCalled();
    expect(result.current.stopGuard).toEqual(dirty);
    await act(async () => {
      await result.current.resolveStopGuard("push");
    });
    expect(stop).toHaveBeenCalledWith("p1", "push");
    expect(result.current.stopGuard).toBeNull();
  });

  it("asks (without a push) when the check itself fails, and Cancel stops nothing", async () => {
    checkUnsavedWork.mockRejectedValue("Project p1 not found");
    const { result } = renderHook(() => useProjectActions(awsProject({ status: "running" })));
    await act(async () => {
      await result.current.handleStop();
    });
    expect(result.current.stopGuard?.checked).toBe(false);
    await act(async () => {
      await result.current.resolveStopGuard(null);
    });
    expect(stop).not.toHaveBeenCalled();
  });

  it("toasts a failed push or StopTask and leaves the project as it was", async () => {
    checkUnsavedWork.mockResolvedValue(dirty);
    stop.mockRejectedValue("Pushing unsaved work failed (exit 1), so the workspace was not stopped:\npush denied");
    const { result } = renderHook(() => useProjectActions(awsProject({ status: "running" })));
    await act(async () => {
      await result.current.handleStop();
    });
    await act(async () => {
      await result.current.resolveStopGuard("push");
    });
    expect(toasts().at(-1)?.detail).toMatch(/push denied/);
  });

  it("never asks for a local project", async () => {
    const { result } = renderHook(() => useProjectActions(localProject({ status: "running" })));
    await act(async () => {
      await result.current.handleStop();
    });
    expect(checkUnsavedWork).not.toHaveBeenCalled();
    expect(stop).toHaveBeenCalledWith("p1");
  });
});

describe("useProjectActions start", () => {
  it("adds the task's last log lines to an AWS start failure", async () => {
    start.mockRejectedValue("The task stopped: Essential container exited");
    awsTaskLog.mockResolvedValue(["entrypoint: clone failed", "bye"]);
    const { result } = renderHook(() => useProjectActions(awsProject()));
    await act(async () => {
      await result.current.handleStart();
    });
    expect(awsTaskLog).toHaveBeenCalledWith("p1", 30);
    expect(toasts().at(-1)?.detail).toMatch(/Last log lines:\nentrypoint: clone failed\nbye/);
  });

  it("does not read the log twice when the error already carries it", async () => {
    start.mockRejectedValue("failed\nLast log lines:\nx");
    const { result } = renderHook(() => useProjectActions(awsProject()));
    await act(async () => {
      await result.current.handleStart();
    });
    expect(awsTaskLog).not.toHaveBeenCalled();
  });
});
