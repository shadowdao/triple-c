import { describe, expect, it } from "vitest";
import { classifyViewerFile } from "./editability";
import type { ViewerFile } from "../lib/types";

const file = (over: Partial<ViewerFile> = {}): ViewerFile => ({
  contents_base64: "", truncated: false, size: 10, hash: "0".repeat(64), editable: true, readonly_reason: null, ...over,
});
const text = new TextEncoder().encode("hello\n");

describe("classifyViewerFile", () => {
  it("text in a write root is editable", () => {
    expect(classifyViewerFile("/workspace/a/x.md", file(), text)).toEqual({ kind: "text", editable: true, reason: null });
  });
  it("a truncated file is read-only and says why", () => {
    const r = classifyViewerFile("/workspace/a/big.log", file({ truncated: true }), text);
    expect(r.editable).toBe(false);
    expect(r.reason).toMatch(/1 MiB/);
  });
  it("Rust's refusal wins and is quoted", () => {
    const r = classifyViewerFile("/etc/hosts", file({ editable: false, readonly_reason: "Only /workspace, /home/claude and /tmp can be written." }), text);
    expect(r).toEqual({ kind: "text", editable: false, reason: "Only /workspace, /home/claude and /tmp can be written." });
  });
  it("images and binaries are never editable", () => {
    expect(classifyViewerFile("/workspace/a/x.png", file(), new Uint8Array([137, 80]))).toMatchObject({ kind: "image", editable: false });
    expect(classifyViewerFile("/workspace/a/x.bin", file(), new Uint8Array([0, 1, 2]))).toMatchObject({ kind: "binary", editable: false });
  });
});
