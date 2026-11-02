import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useAppState } from "../store/appState";
import type { Project } from "../lib/types";

let handler: ((e: { payload: unknown }) => void) | null = null;
const unlisten = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    if (name === "project-status-changed") handler = cb;
    return unlisten;
  }),
}));

import { useProjectStatusEvents } from "./useProjectStatusEvents";

const project = (over: Partial<Project>): Project =>
  ({ id: "p1", name: "demo", status: "running", container_id: "c1", ...over }) as Project;

describe("useProjectStatusEvents", () => {
  beforeEach(() => {
    handler = null;
    unlisten.mockClear();
    useAppState.setState({ projects: [project({}), project({ id: "p2", name: "other" })], toasts: [] });
  });

  it("replaces the project the backend reports, so a dead container stops reading Running", async () => {
    renderHook(() => useProjectStatusEvents());
    await vi.waitFor(() => expect(handler).not.toBeNull());

    handler!({ payload: project({ status: "stopped" }) });

    const { projects, toasts } = useAppState.getState();
    expect(projects.find((p) => p.id === "p1")?.status).toBe("stopped");
    expect(projects.find((p) => p.id === "p2")?.status).toBe("running");
    // The terminals just went dead with no explanation; say why.
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toContain("demo");
    expect(toasts[0].dedupeKey).toBe("container-exited-p1");
  });

  it("stops listening on unmount", async () => {
    const { unmount } = renderHook(() => useProjectStatusEvents());
    await vi.waitFor(() => expect(handler).not.toBeNull());
    unmount();
    expect(unlisten).toHaveBeenCalled();
  });
});
