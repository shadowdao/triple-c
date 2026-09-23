import { describe, expect, it } from "vitest";
import { findFilePathLinks } from "./filePathLinks";

const one = (text: string) => {
  const m = findFilePathLinks(text);
  expect(m, text).toHaveLength(1);
  return m[0];
};

describe("findFilePathLinks — what is a path", () => {
  it.each([
    ["src/foo.ts", "src/foo.ts"],
    ["/workspace/x/README.md", "/workspace/x/README.md"],
    ["./scripts/build.sh", "./scripts/build.sh"],
    ["../other/Cargo.toml", "../other/Cargo.toml"],
    ["Makefile", "Makefile"],
    ["Dockerfile", "Dockerfile"],
    ["CLAUDE.md", "CLAUDE.md"],
    [".gitignore", ".gitignore"],
    ["app/src-tauri/src/lib.rs", "app/src-tauri/src/lib.rs"],
    ["my-dir/some_file.test.tsx", "my-dir/some_file.test.tsx"],
  ])("matches %s", (text, path) => {
    expect(one(text).path).toBe(path);
  });

  it.each([
    "1.2.3",
    "v2.11.0",
    "example.com",
    "claude.ai",
    "e.g.",
    "https://example.com/a/b.ts",
    "http://localhost:1420/viewer.html",
    "foo",
    "a.b",
    "10.0.0.1",
    "and/or",
    "src/components",
  ])("does not match %s", (text) => {
    expect(findFilePathLinks(text)).toEqual([]);
  });

  it("matches a slash-less token only with a known source/doc extension", () => {
    expect(one("index.ts").path).toBe("index.ts");
    expect(one("notes.md").path).toBe("notes.md");
    expect(findFilePathLinks("archive.xyz")).toEqual([]);
    // With a slash, any extension will do.
    expect(one("dist/archive.xyz").path).toBe("dist/archive.xyz");
  });
});

describe("findFilePathLinks — line and column suffixes", () => {
  it("parses :line", () => {
    expect(one("src/foo.ts:42")).toMatchObject({ path: "src/foo.ts", line: 42 });
  });
  it("parses :line:col", () => {
    expect(one("src/foo.ts:42:7")).toMatchObject({ path: "src/foo.ts", line: 42, col: 7 });
  });
  it("parses :start-end", () => {
    expect(one("app/src/lib/urlRelay.ts:139-150")).toMatchObject({ path: "app/src/lib/urlRelay.ts", line: 139, endLine: 150 });
  });
  it("parses #L42 and #L40-L50", () => {
    expect(one("README.md#L42")).toMatchObject({ path: "README.md", line: 42 });
    expect(one("README.md#L40-L50")).toMatchObject({ path: "README.md", line: 40, endLine: 50 });
  });
  it("does not read a trailing colon as a line", () => {
    expect(one("Edited src/foo.ts:")).toMatchObject({ path: "src/foo.ts", line: undefined });
  });
});

describe("findFilePathLinks — markdown wrapping and offsets", () => {
  it.each([
    ["`src/foo.ts`", 1, 11],
    ["(src/foo.ts)", 1, 11],
    ["[src/foo.ts]", 1, 11],
    ['"src/foo.ts"', 1, 11],
    ["'src/foo.ts'", 1, 11],
    ["see src/foo.ts.", 4, 14],
    ["see src/foo.ts, then", 4, 14],
    ["see src/foo.ts;", 4, 14],
  ])("strips wrapping in %s", (text, start, end) => {
    expect(one(text)).toMatchObject({ path: "src/foo.ts", start, end });
  });

  it("keeps the :line suffix inside the span", () => {
    // "at `" is 4 characters; the span covers `src/foo.ts:42` (13 chars).
    expect(one("at `src/foo.ts:42`")).toMatchObject({ path: "src/foo.ts", line: 42, start: 4, end: 17 });
  });

  it("finds several paths in one line, in order", () => {
    const m = findFilePathLinks("Read src/a.ts and src/b.rs:3, wrote docs/c.md");
    expect(m.map((x) => x.path)).toEqual(["src/a.ts", "src/b.rs", "docs/c.md"]);
    expect(m[1].line).toBe(3);
  });

  it("skips anything inside a URL", () => {
    expect(findFilePathLinks("see https://github.com/o/r/blob/main/src/foo.ts:12 now")).toEqual([]);
    expect(one("see https://x.io/a and src/foo.ts").path).toBe("src/foo.ts");
  });

  it("ignores a Claude tool header like ⏺ Read(src/foo.ts) except for the path", () => {
    expect(one("⏺ Read(src/foo.ts)").path).toBe("src/foo.ts");
  });
});
