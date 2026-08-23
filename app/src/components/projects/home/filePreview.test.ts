import { describe, it, expect } from "vitest";
import {
  IMAGE_PREVIEW_LIMIT,
  TEXT_PREVIEW_LIMIT,
  decodeBase64,
  extensionOf,
  imageMimeFor,
  looksBinary,
  previewKind,
  previewLimit,
} from "./filePreview";

describe("extensionOf", () => {
  it("lowercases, and takes only the last segment", () => {
    expect(extensionOf("Photo.PNG")).toBe("png");
    expect(extensionOf("archive.tar.gz")).toBe("gz");
    expect(extensionOf("/workspace/app/main.rs")).toBe("rs");
  });

  it("treats a leading dot as hidden, not as an extension", () => {
    // `.gitignore` is a text file called `.gitignore`, not one of type "gitignore".
    expect(extensionOf(".gitignore")).toBe("");
    expect(extensionOf("Makefile")).toBe("");
  });
});

describe("previewKind", () => {
  it("recognises images by extension, with a MIME the Blob can use", () => {
    expect(previewKind("logo.png")).toBe("image");
    expect(imageMimeFor("logo.PNG")).toBe("image/png");
    expect(imageMimeFor("photo.jpeg")).toBe("image/jpeg");
    expect(imageMimeFor("icon.svg")).toBe("image/svg+xml");
    expect(imageMimeFor("notes.txt")).toBeNull();
  });

  it("recognises known text extensions and conventional extensionless names", () => {
    expect(previewKind("main.rs")).toBe("text");
    expect(previewKind("config.yaml")).toBe("text");
    expect(previewKind("Dockerfile")).toBe("text");
    expect(previewKind("README")).toBe("text");
    expect(previewKind(".gitignore")).toBe("text");
  });

  it("leaves anything else undecided rather than refusing it outright", () => {
    // `unknown` means "read it and sniff the bytes" — a .bak of a config file
    // should still preview.
    expect(previewKind("dump.bak")).toBe("unknown");
    expect(previewKind("app.wasm")).toBe("unknown");
  });
});

describe("previewLimit", () => {
  it("gives images the bigger budget, since they are what blows a text cap", () => {
    expect(previewLimit("photo.jpg")).toBe(IMAGE_PREVIEW_LIMIT);
    expect(previewLimit("notes.md")).toBe(TEXT_PREVIEW_LIMIT);
    expect(previewLimit("mystery.bin")).toBe(TEXT_PREVIEW_LIMIT);
    expect(IMAGE_PREVIEW_LIMIT).toBeGreaterThan(TEXT_PREVIEW_LIMIT);
  });
});

describe("decodeBase64 / looksBinary", () => {
  it("round-trips bytes that are not valid UTF-8", () => {
    // The reason the backend returns base64 at all: these bytes must survive.
    const bytes = decodeBase64(btoa("\xff\xd8\xff\xe0"));
    expect(Array.from(bytes)).toEqual([0xff, 0xd8, 0xff, 0xe0]);
  });

  it("calls a NUL-bearing prefix binary and plain text text", () => {
    expect(looksBinary(new Uint8Array([0x68, 0x69, 0x0a]))).toBe(false);
    expect(looksBinary(new Uint8Array([0x68, 0x00, 0x69]))).toBe(true);
  });

  it("only sniffs the first 8 KB, so a NUL deep in a big file is ignored", () => {
    const bytes = new Uint8Array(20000).fill(0x61);
    bytes[9000] = 0;
    expect(looksBinary(bytes)).toBe(false);
  });
});
