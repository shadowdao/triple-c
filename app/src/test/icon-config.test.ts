import { describe, it, expect } from "vitest";
import { readFileSync, existsSync } from "fs";
import { resolve } from "path";

describe("Window icon configuration", () => {
  const srcTauriDir = resolve(__dirname, "../../src-tauri");

  it("lib.rs sets default_window_icon using the app icon", () => {
    const libRs = readFileSync(resolve(srcTauriDir, "src/lib.rs"), "utf-8");
    expect(libRs).toContain("default_window_icon");
    expect(libRs).toContain("icon.png");
  });

  it("icon.png exists in the icons directory", () => {
    const iconPath = resolve(srcTauriDir, "icons/icon.png");
    expect(existsSync(iconPath)).toBe(true);
  });

  it("icon.ico exists in the icons directory for Windows", () => {
    const icoPath = resolve(srcTauriDir, "icons/icon.ico");
    expect(existsSync(icoPath)).toBe(true);
  });

  it("tauri.conf.json includes icon.ico in bundle icons", () => {
    const config = JSON.parse(
      readFileSync(resolve(srcTauriDir, "tauri.conf.json"), "utf-8")
    );
    expect(config.bundle.icon).toContain("icons/icon.ico");
    expect(config.bundle.icon).toContain("icons/icon.png");
  });
});
