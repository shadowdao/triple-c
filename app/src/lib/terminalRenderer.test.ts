import { describe, it, expect } from "vitest";
import { isLinuxWebview, resolveTerminalGpuRendering } from "./terminalRenderer";

const LINUX = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 Safari/605.1.15";
const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15";
const WINDOWS = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36";
const ANDROID = "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/120 Mobile Safari/537.36";

describe("isLinuxWebview", () => {
  it("recognises desktop Linux", () => {
    expect(isLinuxWebview(LINUX)).toBe(true);
  });

  it("does not count Android as desktop Linux", () => {
    expect(isLinuxWebview(ANDROID)).toBe(false);
  });

  it("rejects the other desktop platforms", () => {
    expect(isLinuxWebview(MAC)).toBe(false);
    expect(isLinuxWebview(WINDOWS)).toBe(false);
  });
});

describe("resolveTerminalGpuRendering", () => {
  it("auto is off on Linux, where WebGL falls back to software rendering", () => {
    expect(resolveTerminalGpuRendering(null, LINUX)).toBe(false);
    expect(resolveTerminalGpuRendering(undefined, LINUX)).toBe(false);
  });

  it("auto is on elsewhere", () => {
    expect(resolveTerminalGpuRendering(null, MAC)).toBe(true);
    expect(resolveTerminalGpuRendering(null, WINDOWS)).toBe(true);
  });

  it("an explicit setting wins on every platform", () => {
    expect(resolveTerminalGpuRendering(true, LINUX)).toBe(true);
    expect(resolveTerminalGpuRendering(false, MAC)).toBe(false);
  });
});
