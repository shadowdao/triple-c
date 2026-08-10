import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useDocker } from "./useDocker";

const checkDocker = vi.fn();
const checkImageExists = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  checkDocker: () => checkDocker(),
  checkImageExists: () => checkImageExists(),
  buildImage: vi.fn(),
  pullImage: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));

const setDockerAvailable = vi.fn();
const setImageExists = vi.fn();

vi.mock("../store/appState", () => ({
  useAppState: (selector: (s: unknown) => unknown) =>
    selector({
      dockerAvailable: false,
      setDockerAvailable,
      imageExists: false,
      setImageExists,
    }),
}));

/** Let the interval fire and its awaited body settle. */
const tick = async () => {
  await act(async () => {
    vi.advanceTimersByTime(5000);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
};

describe("useDocker.startDockerPolling", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    checkImageExists.mockResolvedValue(true);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("runs onAvailable once, after Docker is marked available and the image re-checked", async () => {
    checkDocker.mockResolvedValueOnce(false).mockResolvedValue(true);
    const onAvailable = vi.fn();

    const { result } = renderHook(() => useDocker());
    act(() => {
      result.current.startDockerPolling(onAvailable);
    });

    await tick();
    expect(onAvailable).not.toHaveBeenCalled();

    await tick();
    expect(setDockerAvailable).toHaveBeenCalledWith(true);
    expect(setImageExists).toHaveBeenCalledWith(true);
    expect(onAvailable).toHaveBeenCalledTimes(1);

    // Polling stopped, so no second invocation.
    await tick();
    expect(onAvailable).toHaveBeenCalledTimes(1);
  });

  it("still works without a callback and can be cancelled by its cleanup", async () => {
    checkDocker.mockResolvedValue(true);

    const { result } = renderHook(() => useDocker());
    let stop: () => void = () => {};
    act(() => {
      stop = result.current.startDockerPolling();
    });
    act(() => stop());

    await tick();
    expect(checkDocker).not.toHaveBeenCalled();
    expect(setDockerAvailable).not.toHaveBeenCalled();
  });
});
