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
});
