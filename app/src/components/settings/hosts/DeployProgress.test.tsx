import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, renderHook, screen } from "@testing-library/react";
import DeployProgressView, { runKey, useDeployRuns } from "./DeployProgress";
import type { DeployProgress } from "../../../lib/types";
import { progress, tofuEvent } from "./testkit";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

type Handler = (e: { payload: DeployProgress }) => void;
const handlers = new Set<Handler>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: Handler) => {
    handlers.add(handler);
    return () => handlers.delete(handler);
  }),
}));
function emit(p: DeployProgress) {
  act(() => {
    for (const h of [...handlers]) h({ payload: p });
  });
}

const KEY = "environments/main/terraform.tfstate";

describe("useDeployRuns", () => {
  beforeEach(() => handlers.clear());

  it("tracks runs for the same state key in two regions independently", async () => {
    const onDone = vi.fn();
    const { result } = renderHook(() => useDeployRuns(onDone));
    await act(async () => {
      await result.current.start(KEY, "eu-west-1", "Deploy of main", async () => "eu");
    });
    await act(async () => {
      await result.current.start(KEY, "us-east-1", "Deploy of main", async () => "us");
    });
    expect(handlers.size).toBe(2);
    emit(progress({ op_id: "eu", state_key: KEY, events: [tofuEvent("eu line")] }));
    emit(progress({ op_id: "us", state_key: KEY, events: [tofuEvent("us line")] }));
    emit(progress({ op_id: "us", state_key: KEY, done: true, cancellable: false, result: { ok: true, exit_code: 0, summary: "ok" } }));

    const eu = result.current.runs[runKey("eu-west-1", KEY)];
    const us = result.current.runs[runKey("us-east-1", KEY)];
    expect(eu.events.map((e) => e.message)).toEqual(["eu line"]);
    expect(eu.done).toBe(false);
    expect(us.events.map((e) => e.message)).toEqual(["us line"]);
    expect(us.done).toBe(true);
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(onDone).toHaveBeenCalledWith({ stateKey: KEY, region: "us-east-1", result: { ok: true, exit_code: 0, summary: "ok" } });
    // The finished run's listener is gone; the other region's is still there.
    expect(handlers.size).toBe(1);
  });

  it("removes its progress listeners on unmount", async () => {
    const { result, unmount } = renderHook(() => useDeployRuns());
    await act(async () => {
      await result.current.start(KEY, "eu-west-1", "Deploy of main", async () => "a");
      await result.current.start("boundary/terraform.tfstate", "eu-west-1", "Boundary", async () => "b");
    });
    expect(handlers.size).toBe(2);
    unmount();
    expect(handlers.size).toBe(0);
  });

  it("a rejected start removes its listener and returns the AWS error", async () => {
    const { result } = renderHook(() => useDeployRuns());
    let err: unknown;
    await act(async () => {
      err = await result.current.start(KEY, "eu-west-1", "Deploy of main", async () => {
        throw { kind: "other", message: "A deployment is already running" };
      });
    });
    expect(err).toEqual({ kind: "other", message: "A deployment is already running" });
    expect(handlers.size).toBe(0);
    expect(result.current.runs).toEqual({});
  });
});

describe("DeployProgress layout", () => {
  it("wraps its header and a long failure summary", () => {
    render(
      <DeployProgressView
        connectionId="h1"
        run={{
          stateKey: KEY,
          region: "eu-west-1",
          label: "Deploy of main",
          opId: "x",
          events: [],
          done: true,
          cancellable: false,
          result: { ok: false, exit_code: 1, summary: "a very long summary ".repeat(10) },
        }}
      />,
    );
    const header = screen.getByTestId("deploy-progress-header");
    expect(header.className).toContain("flex-wrap");
    expect(header.firstElementChild?.className).toContain("min-w-0");
    expect(screen.getByRole("status").firstElementChild?.className).toContain("whitespace-normal");
  });
});
