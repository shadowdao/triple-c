import { describe, expect, it } from "vitest";
import {
  errorText,
  FILE_EXISTS_MARKER,
  fileExistsPath,
  isFileExistsError,
  readableRefusal,
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

/**
 * The other half of the contract: refusals that are *not* a name clash, but are
 * a sentence the backend wrote for the person reading it. They used to arrive
 * as a toast's `detail`, which renders as collapsed monospace behind a
 * "Details" button — so the only part of the message that explained anything
 * was the part nobody saw.
 */
describe("readableRefusal", () => {
  const hidden =
    '".ssh" is a hidden folder — Triple-C will not save there. Choose a visible location.';
  const outside =
    "Folder path is outside the folders this panel can change (/workspace, /home/claude, /tmp): /etc";

  it("recognises the hidden-host-folder refusal, in both directions", () => {
    expect(readableRefusal(hidden)).toBe(hidden);
    expect(
      readableRefusal('".aws" is a hidden folder — Triple-C will not read there. Choose a visible location.'),
    ).toContain("hidden folder");
  });

  it("recognises the container write-root refusal", () => {
    expect(readableRefusal(outside)).toBe(outside);
  });

  it("strips a wrapper a JS layer put in front of the sentence", () => {
    // `invoke` rejects with the bare string today, but an `Error` anywhere in
    // between would otherwise put "Error: " in front of prose meant to be read.
    expect(readableRefusal(new Error(hidden))).toBe(hidden);
    expect(readableRefusal(`Error: ${hidden}`)).toBe(hidden);
    expect(readableRefusal(`Uncaught (in promise) Error: ${outside}`)).toBe(outside);
    expect(readableRefusal({ message: `invoke failed: ${outside}` })).toBe(outside);
  });

  it("says nothing about failures that are not a written refusal", () => {
    // Promotion is an improvement, not a fallback: anything unrecognised keeps
    // reporting exactly as it did before.
    expect(readableRefusal("File too large to upload (900 MB; limit 256 MB)")).toBeNull();
    expect(readableRefusal("FILE_EXISTS: /workspace/a.txt already exists")).toBeNull();
    expect(readableRefusal("cp: Permission denied")).toBeNull();
    expect(readableRefusal(null)).toBeNull();
  });
});

describe("errorText", () => {
  it("keeps an ordinary message intact", () => {
    expect(errorText("cp: cannot create regular file: Permission denied")).toBe(
      "cp: cannot create regular file: Permission denied",
    );
  });

  it("reads a message out of a shape `String()` would render as [object Object]", () => {
    expect(errorText({ message: "Container not running" })).toBe("Container not running");
    expect(errorText({ kind: "NotRunning" })).toBe("NotRunning");
    expect(errorText(new Error("Failed to upload file to container: no space left"))).toBe(
      "Failed to upload file to container: no space left",
    );
  });

  it("prefers the written refusal when there is one", () => {
    expect(errorText(new Error("Folder path is outside the folders this panel can change (/workspace): /etc"))).toBe(
      "Folder path is outside the folders this panel can change (/workspace): /etc",
    );
  });
});
