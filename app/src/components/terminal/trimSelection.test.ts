import { describe, it, expect } from "vitest";
import { trimSelection } from "./trimSelection";

describe("trimSelection", () => {
  it("returns empty string unchanged", () => {
    expect(trimSelection("")).toBe("");
  });

  it("trims leading and trailing whitespace on a single line", () => {
    expect(trimSelection("    hello    ")).toBe("hello");
  });

  it("dedents common leading whitespace while preserving inner indent", () => {
    const input = "    def foo():\n        return 1\n";
    expect(trimSelection(input)).toBe("def foo():\n    return 1");
  });

  it("strips leading and trailing blank lines", () => {
    const input = "\n\n  hello\n\n";
    expect(trimSelection(input)).toBe("hello");
  });

  it("preserves interior blank lines", () => {
    const input = "  line1\n\n  line2";
    expect(trimSelection(input)).toBe("line1\n\nline2");
  });

  it("is idempotent on already-clean text", () => {
    const clean = "def foo():\n    return 1";
    expect(trimSelection(clean)).toBe(clean);
    expect(trimSelection(trimSelection(clean))).toBe(clean);
  });

  it("ignores blank lines when computing the common indent", () => {
    // The blank line has 0 leading whitespace but shouldn't force minIndent to 0.
    const input = "    a\n\n    b";
    expect(trimSelection(input)).toBe("a\n\nb");
  });

  it("strips trailing whitespace per line", () => {
    const input = "alpha   \nbeta\t\t\ngamma";
    expect(trimSelection(input)).toBe("alpha\nbeta\ngamma");
  });

  it("handles mixed-width padding (pads to min)", () => {
    const input = "    one\n  two\n      three";
    // minIndent = 2
    expect(trimSelection(input)).toBe("  one\ntwo\n    three");
  });

  it("handles tabs as leading whitespace", () => {
    const input = "\tfoo\n\t\tbar";
    // minIndent = 1 tab
    expect(trimSelection(input)).toBe("foo\n\tbar");
  });

  it("returns empty when input is only whitespace", () => {
    expect(trimSelection("   \n  \n")).toBe("");
  });

  it("leaves a zero-indent line alone (no false dedent)", () => {
    const input = "no-indent\n  indented";
    expect(trimSelection(input)).toBe("no-indent\n  indented");
  });
});
