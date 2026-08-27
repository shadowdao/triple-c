import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useProjects } from "./useProjects";
import { useAppState } from "../store/appState";
import type { Project, ProjectStatus } from "../lib/types";

const startProjectContainer = vi.fn();
const stopProjectContainer = vi.fn();
const rebuildProjectContainer = vi.fn();
const listProjects = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  startProjectContainer: (id: string) => startProjectContainer(id),
  stopProjectContainer: (id: string) => stopProjectContainer(id),
  rebuildProjectContainer: (id: string) => rebuildProjectContainer(id),
  listProjects: () => listProjects(),
  addProject: vi.fn(),
  removeProject: vi.fn(),
  updateProject: vi.fn(),
}));

const project = (status: ProjectStatus): Project =>
  ({ id: "p1", name: "whp", status }) as unknown as Project;

/** The status the sidebar row and Project Home both read. */
const statusOf = () => useAppState.getState().projects.find((p) => p.id === "p1")?.status;

beforeEach(() => {
  vi.clearAllMocks();
  useAppState.setState({ projects: [project("running")] });
});

/**
 * The optimistic status is what makes a click move the row immediately. It is
 * also what strands the row when the command it was betting on never ran.
 *
 * Since every lifecycle command started taking the per-project lock and failing
 * fast, all three can be refused *before* the backend changes anything — and
 * `stop` could not fail this way at all before, because it took no exclusion.
 * `isTransitioning` disables both Start and Stop, and the only thing that
 * clears it is `reconcileProjectStatuses`, which runs once when Docker first
 * appears. So a stale optimistic status is not a cosmetic problem: it is a row
 * the user cannot operate again until the app is restarted.
 */
describe("useProjects puts the status back when a refused command never ran", () => {
  const refusal =
    "This project's snapshot is being compacted. Wait for it to finish before starting or recreating its container.";

  it("does not leave a refused Stop showing 'stopping' forever", async () => {
    stopProjectContainer.mockRejectedValue(refusal);
    // The lock is taken before `update_status`, so the backend still holds the
    // truth — which is why re-reading it is the correction, not a guess.
    listProjects.mockResolvedValue([project("running")]);

    const { result } = renderHook(() => useProjects());
    await act(async () => {
      await expect(result.current.stop("p1")).rejects.toBe(refusal);
    });

    expect(statusOf()).toBe("running");
  });

  it("does not leave a refused Start showing 'starting' forever", async () => {
    useAppState.setState({ projects: [project("stopped")] });
    startProjectContainer.mockRejectedValue(refusal);
    listProjects.mockResolvedValue([project("stopped")]);

    const { result } = renderHook(() => useProjects());
    await act(async () => {
      await expect(result.current.start("p1")).rejects.toBe(refusal);
    });

    expect(statusOf()).toBe("stopped");
  });

  it("does not leave a refused Reset showing 'starting' forever", async () => {
    rebuildProjectContainer.mockRejectedValue(refusal);
    listProjects.mockResolvedValue([project("running")]);

    const { result } = renderHook(() => useProjects());
    await act(async () => {
      await expect(result.current.rebuild("p1")).rejects.toBe(refusal);
    });

    expect(statusOf()).toBe("running");
  });

  it("falls back to what was on screen when even the re-read fails", async () => {
    // Two failures in a row must not land on the one state there is no way out
    // of. The backend's answer is preferred, but "unknown" is never a reason to
    // keep showing a transition that is not happening.
    stopProjectContainer.mockRejectedValue(refusal);
    listProjects.mockRejectedValue("Docker is not running");

    const { result } = renderHook(() => useProjects());
    await act(async () => {
      await expect(result.current.stop("p1")).rejects.toBe(refusal);
    });

    expect(statusOf()).toBe("running");
  });

  it("prefers the backend's answer over the status it captured", async () => {
    // A start that dies half-way really has changed the world, so the captured
    // status would be a lie. `listProjects` is the thing that knows.
    useAppState.setState({ projects: [project("stopped")] });
    startProjectContainer.mockRejectedValue("container exited during startup");
    listProjects.mockResolvedValue([project("error")]);

    const { result } = renderHook(() => useProjects());
    await act(async () => {
      await expect(result.current.start("p1")).rejects.toBe(
        "container exited during startup",
      );
    });

    expect(statusOf()).toBe("error");
  });

  it("still paints the optimistic status on the way in", async () => {
    let release: (() => void) | undefined;
    stopProjectContainer.mockReturnValue(
      new Promise<void>((resolve) => {
        release = resolve;
      }),
    );
    listProjects.mockResolvedValue([project("stopped")]);

    const { result } = renderHook(() => useProjects());
    let pending: Promise<void> | undefined;
    act(() => {
      pending = result.current.stop("p1");
    });
    expect(statusOf()).toBe("stopping");

    await act(async () => {
      release?.();
      await pending;
    });
    expect(statusOf()).toBe("stopped");
  });
});

describe("useProjects.rebuild on success", () => {
  it("puts the outcome's project, not the whole outcome, into the list", async () => {
    const rebuilt = project("running");
    rebuildProjectContainer.mockResolvedValue({
      project: rebuilt,
      leftover_image: null,
      leftover_volumes: [],
    });

    const { result } = renderHook(() => useProjects());
    let outcome!: Awaited<ReturnType<typeof result.current.rebuild>>;
    await act(async () => {
      outcome = await result.current.rebuild("p1");
    });

    // A regression here would put the `{ project, leftover_image,
    // leftover_volumes }` wrapper into the projects list instead of the
    // `Project` it wraps — a shape mismatch `tsc` would not catch inside a
    // callback typed to take `unknown` per Tauri's `invoke`.
    expect(useAppState.getState().projects.find((p) => p.id === "p1")).toEqual(rebuilt);
    expect(outcome.leftover_volumes).toEqual([]);
  });
});
