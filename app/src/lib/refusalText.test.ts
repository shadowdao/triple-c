import { describe, expect, it } from "vitest";
import { errorText, readableRefusal } from "./refusalText";

/**
 * Refusals that are a sentence the backend wrote for the person reading it.
 * They used to arrive as a toast's `detail`, which renders as collapsed
 * monospace behind a "Details" button — so the only part of the message that
 * explained anything was the part nobody saw.
 */
describe("readableRefusal", () => {
  const hidden =
    'the path goes through ".ssh", a hidden folder — Triple-C will not save anything whose folders are not all visible. Choose a visible location.';
  const outside =
    "Folder path is outside the folders this panel can change (/workspace, /home/claude, /tmp): /etc";

  it("recognises the hidden-host-folder refusal, in both directions", () => {
    expect(readableRefusal(hidden)).toBe(hidden);
    expect(
      readableRefusal(
        'the path goes through ".aws", a hidden folder — Triple-C will not read anything whose folders are not all visible. Choose a visible location.',
      ),
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
