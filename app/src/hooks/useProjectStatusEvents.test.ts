import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useAppState } from "../store/appState";
import type { Project } from "../lib/types";

let handler: ((e: { payload: unknown }) => void) | null = null;
const unlisten = vi.fn();
const listen = vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
  if (name === "project-container-exited") handler = cb;
  return unlisten;
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => listen(name, cb),
}));

import { useProjectStatusEvents } from "./useProjectStatusEvents";

const project = (over: Partial<Project>): Project =>
  ({ id: "p1", name: "demo", status: "running", container_id: "c1", ...over }) as Project;

describe("useProjectStatusEvents", () => {
  beforeEach(() => {
    handler = null;
    unlisten.mockClear();
    listen.mockClear();
    useAppState.setState({ projects: [project({}), project({ id: "p2", name: "other" })], toasts: [] });
  });

  it("applies a live exit and says why the sessions just died", async () => {
    renderHook(() => useProjectStatusEvents());
    await vi.waitFor(() => expect(handler).not.toBeNull());

    handler!({ payload: { project: project({ status: "stopped" }), reason: "exited" } });

    const { projects, toasts } = useAppState.getState();
    expect(projects.find((p) => p.id === "p1")?.status).toBe("stopped");
    expect(projects.find((p) => p.id === "p2")?.status).toBe("running");
    expect(toasts).toHaveLength(1);
    expect(toasts[0].kind).toBe("error");
    expect(toasts[0].message).toContain("demo");
    expect(toasts[0].dedupeKey).toBe("container-exited-p1");
  });

  it("applies a container found already stopped at startup silently", async () => {
    // After a host reboot every project's container is down before the app
    // opens; nothing was interrupted this run, so nothing to alarm anyone about.
    renderHook(() => useProjectStatusEvents());
    await vi.waitFor(() => expect(handler).not.toBeNull());

    handler!({ payload: { project: project({ status: "stopped" }), reason: "found_stopped" } });

    const { projects, toasts } = useAppState.getState();
    expect(projects.find((p) => p.id === "p1")?.status).toBe("stopped");
    expect(toasts).toHaveLength(0);
  });

  it("stops listening on unmount", async () => {
    const { unmount } = renderHook(() => useProjectStatusEvents());
    await vi.waitFor(() => expect(handler).not.toBeNull());
    unmount();
    expect(unlisten).toHaveBeenCalled();
  });

  it("survives listen() rejecting (no unhandled rejection)", async () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    listen.mockImplementationOnce(async () => {
      throw new Error("no IPC");
    });
    const { unmount } = renderHook(() => useProjectStatusEvents());
    await vi.waitFor(() => expect(err).toHaveBeenCalled());
    unmount();
    err.mockRestore();
  });
});
