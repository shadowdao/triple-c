import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useDocker } from "./useDocker";

const getDockerStatus = vi.fn();
const checkImageExists = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  getDockerStatus: () => getDockerStatus(),
  checkImageExists: () => checkImageExists(),
  buildImage: vi.fn(),
  pullImage: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));

const setDockerAvailable = vi.fn();
const setDockerStatus = vi.fn();
const status = (state: string) => ({ state, endpoint: "unix:///var/run/docker.sock", detail: null, os: "linux" });
const setImageExists = vi.fn();

vi.mock("../store/appState", () => ({
  useAppState: (selector: (s: unknown) => unknown) =>
    selector({
      dockerAvailable: false,
      setDockerAvailable,
      dockerStatus: null,
      setDockerStatus,
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
    getDockerStatus.mockResolvedValueOnce(status("not_running")).mockResolvedValue(status("running"));
    const onAvailable = vi.fn();

    const { result } = renderHook(() => useDocker());
    act(() => {
      result.current.startDockerPolling(onAvailable);
    });

    await tick();
    expect(onAvailable).not.toHaveBeenCalled();
    // The not-running state is published so the dialog can show it live.
    expect(setDockerStatus).toHaveBeenLastCalledWith(status("not_running"));

    await tick();
    expect(setDockerStatus).toHaveBeenLastCalledWith(status("running"));
    expect(setImageExists).toHaveBeenCalledWith(true);
    expect(onAvailable).toHaveBeenCalledTimes(1);

    // Polling stopped, so no second invocation.
    await tick();
    expect(onAvailable).toHaveBeenCalledTimes(1);
  });

  it("still works without a callback and can be cancelled by its cleanup", async () => {
    getDockerStatus.mockResolvedValue(status("running"));

    const { result } = renderHook(() => useDocker());
    let stop: () => void = () => {};
    act(() => {
      stop = result.current.startDockerPolling();
    });
    act(() => stop());

    await tick();
    expect(getDockerStatus).not.toHaveBeenCalled();
    expect(setDockerStatus).not.toHaveBeenCalled();
  });
});
