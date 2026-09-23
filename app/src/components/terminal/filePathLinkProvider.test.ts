import { describe, expect, it, vi } from "vitest";
import { createFilePathLinkProvider } from "./filePathLinkProvider";

const fakeTerm = (rows: Array<[string, boolean]>) => ({
  buffer: {
    active: {
      getLine: (y: number) => rows[y] && { isWrapped: rows[y][1], translateToString: () => rows[y][0] },
    },
  },
}) as unknown as Parameters<typeof createFilePathLinkProvider>[0];

describe("createFilePathLinkProvider", () => {
  it("reports 1-based inclusive ranges and activates through the gate", () => {
    const onOpen = vi.fn();
    const gate = vi.fn(() => true);
    const provider = createFilePathLinkProvider(fakeTerm([["Edited src/foo.ts:42 today", false]]), onOpen, gate);
    const links = vi.fn();
    provider.provideLinks(1, links);
    const [list] = links.mock.calls[0];
    expect(list).toHaveLength(1);
    expect(list[0].range).toEqual({ start: { x: 8, y: 1 }, end: { x: 20, y: 1 } });
    expect(list[0].text).toBe("src/foo.ts:42");
    list[0].activate(new MouseEvent("click"), list[0].text);
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ path: "src/foo.ts", line: 42 }));
  });

  it("does nothing when the gate refuses", () => {
    const onOpen = vi.fn();
    const provider = createFilePathLinkProvider(fakeTerm([["src/foo.ts", false]]), onOpen, () => false);
    const links = vi.fn();
    provider.provideLinks(1, links);
    links.mock.calls[0][0][0].activate(new MouseEvent("click"), "src/foo.ts");
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("spans a wrapped path across rows", () => {
    const provider = createFilePathLinkProvider(fakeTerm([["see /workspace/p/", false], ["src/foo.ts:7", true]]), vi.fn(), () => true);
    const links = vi.fn();
    provider.provideLinks(2, links);
    expect(links.mock.calls[0][0][0].range).toEqual({ start: { x: 5, y: 1 }, end: { x: 12, y: 2 } });
  });

  it("answers undefined for a row with nothing", () => {
    const provider = createFilePathLinkProvider(fakeTerm([["plain words", false]]), vi.fn(), () => true);
    const links = vi.fn();
    provider.provideLinks(1, links);
    expect(links).toHaveBeenCalledWith(undefined);
  });

  it("answers undefined for a row past the end of the buffer", () => {
    const provider = createFilePathLinkProvider(fakeTerm([["src/foo.ts", false]]), vi.fn(), () => true);
    const links = vi.fn();
    provider.provideLinks(5, links);
    expect(links).toHaveBeenCalledWith(undefined);
  });

  it("hands the hover the raw path, relative as printed (preflight P6)", () => {
    const hover = { show: vi.fn(), hide: vi.fn() };
    const provider = createFilePathLinkProvider(
      fakeTerm([["Edited src/foo.ts:42 today", false]]), vi.fn(), () => true, hover,
    );
    const links = vi.fn();
    provider.provideLinks(1, links);
    const link = links.mock.calls[0][0][0];
    link.hover(new MouseEvent("mousemove"), link.text);
    expect(hover.show).toHaveBeenCalledWith("src/foo.ts");
    link.leave(new MouseEvent("mouseout"), link.text);
    expect(hover.hide).toHaveBeenCalled();
  });
});
