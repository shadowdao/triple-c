import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useAppState } from "../store/appState";

let handler: ((e: { payload: unknown }) => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, cb: (e: { payload: unknown }) => void) => {
    if (name === "workspace-warning") handler = cb;
    return () => {};
  },
}));

import { useWorkspaceWarnings } from "./useWorkspaceWarnings";

describe("useWorkspaceWarnings", () => {
  beforeEach(() => {
    handler = null;
    useAppState.setState({ toasts: [] });
  });

  it("shows a start's clone/setup warning as a warning toast with the last lines", async () => {
    renderHook(() => useWorkspaceWarnings());
    await vi.waitFor(() => expect(handler).not.toBeNull());
    const fire = () =>
      handler!({
        payload: {
          project_id: "p1",
          message: "“demo” started, but not everything went well",
          detail: "The setup script failed (exit 2).\nLast lines:\nE: oops",
        },
      });
    fire();
    fire();
    const { toasts } = useAppState.getState();
    // A second start's warning replaces the first.
    expect(toasts).toHaveLength(1);
    expect(toasts[0].kind).toBe("warning");
    expect(toasts[0].message).toContain("demo");
    expect(toasts[0].detail).toContain("E: oops");
  });
});
