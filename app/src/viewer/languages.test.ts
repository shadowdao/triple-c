import { describe, expect, it } from "vitest";
import { languageFor, wrapsLines } from "./languages";

describe("languageFor", () => {
  it.each(["a.ts", "a.tsx", "a.js", "a.jsx", "a.mjs", "a.rs", "a.py", "a.json", "a.yaml", "a.yml", "a.toml", "a.sh", "a.bash", "a.css", "a.html", "a.md", "Dockerfile", "Cargo.lock", "README"])(
    "resolves %s without throwing", async (name) => {
      const result = await languageFor(`/workspace/${name}`);
      if (name === "README") {
        expect(result).toBeNull();
      } else {
        expect(result).not.toBeNull();
      }
    });
  it("returns null for an unknown extension", async () => {
    await expect(languageFor("/workspace/x.xyz")).resolves.toBeNull();
  });
  it("returns an extension for markdown", async () => {
    await expect(languageFor("/workspace/x.md")).resolves.not.toBeNull();
  });
});

describe("wrapsLines", () => {
  it("wraps prose, not code", () => {
    expect(wrapsLines("x.md")).toBe(true);
    expect(wrapsLines("x.txt")).toBe(true);
    expect(wrapsLines("x.rs")).toBe(false);
  });
});
