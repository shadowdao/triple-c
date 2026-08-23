import { describe, expect, it } from "vitest";
import {
  FILE_EXISTS_MARKER,
  fileExistsPath,
  isFileExistsError,
} from "./uploadErrors";

/**
 * The shapes here are the point of the module.
 *
 * A Tauri command error crosses the IPC boundary as whatever `serde` made of
 * it, and the Rust side is free to change from `Err(String)` to a serialised
 * error enum without anyone thinking of this file. Every one of these has to
 * keep meaning "that name is taken", or an upload that could have been
 * retried with `overwrite: true` degrades into a raw string in a toast.
 */
describe("isFileExistsError", () => {
  it("recognises the agreed prose form", () => {
    expect(isFileExistsError("FILE_EXISTS: /workspace/notes.txt already exists")).toBe(true);
  });

  it("recognises a bare marker", () => {
    expect(isFileExistsError(FILE_EXISTS_MARKER)).toBe(true);
  });

  it("recognises a serialised error enum, whatever case it is written in", () => {
    expect(isFileExistsError({ kind: "FileExists", path: "/workspace/a.txt" })).toBe(true);
    expect(isFileExistsError({ code: "file-exists" })).toBe(true);
    expect(isFileExistsError({ type: "file_exists" })).toBe(true);
  });

  it("recognises it inside a message field", () => {
    expect(isFileExistsError({ message: "upload refused: FILE_EXISTS" })).toBe(true);
    expect(isFileExistsError(new Error("FILE_EXISTS: /workspace/a.txt"))).toBe(true);
  });

  it("looks one level into a wrapped error", () => {
    expect(isFileExistsError({ error: { kind: "FileExists" } })).toBe(true);
  });

  it("says no to every other failure, which must not raise an overwrite prompt", () => {
    expect(isFileExistsError("File too large to upload (900 MB; limit 256 MB)")).toBe(false);
    expect(isFileExistsError("cp: cannot create regular file: Permission denied")).toBe(false);
    expect(isFileExistsError({ kind: "NotRunning" })).toBe(false);
    expect(isFileExistsError(null)).toBe(false);
    expect(isFileExistsError(undefined)).toBe(false);
    expect(isFileExistsError(42)).toBe(false);
    expect(isFileExistsError({})).toBe(false);
  });
});

describe("fileExistsPath", () => {
  it("reads the path out of the agreed prose form", () => {
    expect(fileExistsPath("FILE_EXISTS: /workspace/notes.txt already exists")).toBe(
      "/workspace/notes.txt",
    );
  });

  it("prefers a structured field", () => {
    expect(fileExistsPath({ kind: "FileExists", path: "/workspace/a.txt" })).toBe(
      "/workspace/a.txt",
    );
    expect(fileExistsPath({ kind: "FileExists", container_path: "/workspace/b.txt" })).toBe(
      "/workspace/b.txt",
    );
  });

  it("finds one in a wrapped error", () => {
    expect(fileExistsPath({ error: { kind: "FileExists", path: "/workspace/c.txt" } })).toBe(
      "/workspace/c.txt",
    );
  });

  it("returns null rather than guessing", () => {
    // The caller falls back to the host path it was uploading, which is always
    // known — so "no path" is a perfectly good answer.
    expect(fileExistsPath("FILE_EXISTS")).toBeNull();
    expect(fileExistsPath({ kind: "FileExists" })).toBeNull();
    expect(fileExistsPath(null)).toBeNull();
  });
});
