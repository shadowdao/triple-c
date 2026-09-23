import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useViewerPolling } from "./useViewerPolling";

describe("useViewerPolling", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const setVisibility = (state: DocumentVisibilityState) => {
    Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  };

  it("ticks on the interval only while visible, and once immediately on becoming visible", async () => {
    setVisibility("visible");
    const tick = vi.fn(async () => {});
    renderHook(() => useViewerPolling(2000, tick, true));
    expect(tick).toHaveBeenCalledTimes(1); // initial
    await vi.advanceTimersByTimeAsync(4000);
    expect(tick).toHaveBeenCalledTimes(3);
    setVisibility("hidden");
    await vi.advanceTimersByTimeAsync(6000);
    expect(tick).toHaveBeenCalledTimes(3);
    setVisibility("visible");
    expect(tick).toHaveBeenCalledTimes(4);
  });

  it("does not overlap ticks and stops when disabled", async () => {
    setVisibility("visible");
    let resolve: () => void = () => {};
    const tick = vi.fn(() => new Promise<void>((r) => { resolve = r; }));
    const { rerender } = renderHook(({ on }) => useViewerPolling(1000, tick, on), { initialProps: { on: true } });
    await vi.advanceTimersByTimeAsync(3000);
    expect(tick).toHaveBeenCalledTimes(1);
    resolve();
    await vi.advanceTimersByTimeAsync(1000);
    expect(tick).toHaveBeenCalledTimes(2);
    rerender({ on: false });
    resolve();
    await vi.advanceTimersByTimeAsync(5000);
    expect(tick).toHaveBeenCalledTimes(2);
  });
});
