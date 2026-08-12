import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import AutomationTab from "./AutomationTab";
import type { Project, ScheduledTask } from "../../../lib/types";

const listScheduledTasks = vi.fn(async () => tasks);
const getSchedulerNotifications = vi.fn(async () => []);
const runScheduledTaskNow = vi.fn(async () => "started");
const pushToast = vi.fn();

vi.mock("../../../lib/tauri-commands", () => ({
  listScheduledTasks: () => listScheduledTasks(),
  getSchedulerNotifications: () => getSchedulerNotifications(),
  runScheduledTaskNow: (p: string, t: string) => runScheduledTaskNow(p, t),
  clearSchedulerNotifications: vi.fn(async () => {}),
  getScheduledTaskLog: vi.fn(async () => ""),
  removeScheduledTask: vi.fn(async () => {}),
  setScheduledTaskEnabled: vi.fn(async () => {}),
}));

vi.mock("../../../store/appState", () => ({
  useAppState: (selector: (s: unknown) => unknown) => selector({ pushToast }),
}));

const project = { id: "p1", name: "api", status: "running" } as unknown as Project;

const baseTask: ScheduledTask = {
  id: "a1b2c3d4",
  name: "nightly",
  prompt: "Run the suite",
  schedule: "0 3 * * *",
  task_type: "recurring",
  at: null,
  enabled: true,
  working_dir: "/workspace",
  created_at: null,
  last_run: null,
  next_run: null,
  running: false,
  running_since: null,
};

let tasks: ScheduledTask[] = [];

async function renderTab() {
  render(<AutomationTab project={project} />);
  await act(async () => {
    await Promise.resolve();
  });
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  tasks = [baseTask];
  listScheduledTasks.mockClear();
  runScheduledTaskNow.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("AutomationTab run state", () => {
  it("offers Run now for an idle task and says nothing about running", async () => {
    await renderTab();
    expect(screen.getByRole("button", { name: "Run now" })).toBeEnabled();
    expect(screen.queryByText(/Running/)).toBeNull();
  });

  it("shows a running task as running, with elapsed time, and blocks a second trigger", async () => {
    const startedSecondsAgo = new Date(Date.now() - 90_000).toISOString();
    tasks = [{ ...baseTask, running: true, running_since: startedSecondsAgo }];
    await renderTab();

    // The whole point: a detached run is visible rather than silent.
    expect(screen.getByText(/Running for 1m/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Running…" })).toBeDisabled();
  });

  it("keeps polling after a trigger, so a run that has not registered yet still appears", async () => {
    await renderTab();
    const callsAfterLoad = listScheduledTasks.mock.calls.length;

    // The runner needs a moment to write its state file; until then the task
    // still reads as idle, which is exactly the window that used to look dead.
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Run now" }));
      await Promise.resolve();
    });
    expect(runScheduledTaskNow).toHaveBeenCalledWith("p1", "a1b2c3d4");

    tasks = [{ ...baseTask, running: true, running_since: new Date().toISOString() }];
    await act(async () => {
      vi.advanceTimersByTime(2000);
      await Promise.resolve();
    });

    expect(listScheduledTasks.mock.calls.length).toBeGreaterThan(callsAfterLoad);
    expect(screen.getByRole("button", { name: "Running…" })).toBeDisabled();
  });

  it("stops polling once nothing is running", async () => {
    await renderTab();
    // No trigger, nothing running: the interval must not be armed at all.
    const before = listScheduledTasks.mock.calls.length;
    await act(async () => {
      vi.advanceTimersByTime(30_000);
      await Promise.resolve();
    });
    expect(listScheduledTasks.mock.calls.length).toBe(before);
  });
});
