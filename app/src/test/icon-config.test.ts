import { describe, it, expect } from "vitest";
import { readFileSync, existsSync } from "fs";
import { resolve } from "path";

describe("Window icon configuration", () => {
  const srcTauriDir = resolve(__dirname, "../../src-tauri");

  it("lib.rs sets window icon using set_icon in setup hook", () => {
    const libRs = readFileSync(resolve(srcTauriDir, "src/lib.rs"), "utf-8");
    expect(libRs).toContain("set_icon");
    expect(libRs).toContain("icon.png");
  });

  it("Cargo.toml enables image-png feature for icon loading", () => {
    const cargoToml = readFileSync(resolve(srcTauriDir, "Cargo.toml"), "utf-8");
    expect(cargoToml).toContain("image-png");
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

  it("icon.ico carries the small sizes Windows draws in the taskbar", () => {
    // A single-image .ico is the taskbar bug: Windows upscales 16x16 into every
    // other slot. Regenerate with `python3 branding/build-icons.py`.
    const ico = readFileSync(resolve(srcTauriDir, "icons/icon.ico"));
    const count = ico.readUInt16LE(4);
    expect(count).toBeGreaterThan(1);

    // Directory entries start at byte 6; width/height of 0 means 256.
    const widths = new Set<number>();
    for (let i = 0; i < count; i++) {
      const w = ico[6 + i * 16];
      widths.add(w === 0 ? 256 : w);
    }
    for (const size of [16, 24, 32, 48, 256]) {
      expect(widths).toContain(size);
    }
  });

  it("icon.icns exists and is bundled for macOS", () => {
    const icnsPath = resolve(srcTauriDir, "icons/icon.icns");
    expect(existsSync(icnsPath)).toBe(true);
    expect(readFileSync(icnsPath).subarray(0, 4).toString("ascii")).toBe("icns");

    const config = JSON.parse(
      readFileSync(resolve(srcTauriDir, "tauri.conf.json"), "utf-8")
    );
    expect(config.bundle.icon).toContain("icons/icon.icns");
  });
});
