import { describe, it, expect, vi, afterEach } from "vitest";
import { dragPreviewIcon } from "./dragPreview";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("dragPreviewIcon", () => {
  it("falls back to a PNG data URL when there is no 2D context", () => {
    // jsdom has no canvas, and a webview can refuse one. `startDrag` requires
    // an image and the Rust side accepts nothing but a PNG data URL, so a
    // fallback that is not one takes the whole drag down with it.
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    expect(dragPreviewIcon("notes.txt")).toMatch(/^data:image\/png;base64,[A-Za-z0-9+/=]+$/);
  });

  it("falls back rather than throwing when the canvas throws", () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(() => {
      throw new Error("no canvas here");
    });
    expect(dragPreviewIcon("notes.txt")).toMatch(/^data:image\/png;base64,/);
  });

  it("refuses a canvas that encoded nothing", () => {
    // jsdom's `toDataURL` answers `data:,` — which the Rust side rejects
    // outright, so returning it would be worse than not drawing at all.
    const ctx = {
      scale: vi.fn(),
      measureText: () => ({ width: 60 }),
      beginPath: vi.fn(),
      roundRect: vi.fn(),
      fill: vi.fn(),
      stroke: vi.fn(),
      fillRect: vi.fn(),
      strokeRect: vi.fn(),
      fillText: vi.fn(),
      font: "",
      fillStyle: "",
      strokeStyle: "",
      lineWidth: 0,
      textBaseline: "",
    } as unknown as CanvasRenderingContext2D;
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(ctx);
    vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue("data:,");

    expect(dragPreviewIcon("notes.txt")).toMatch(/^data:image\/png;base64,[A-Za-z0-9+/=]+$/);
  });

  it("uses what the canvas drew when there is one", () => {
    const ctx = {
      scale: vi.fn(),
      measureText: () => ({ width: 60 }),
      beginPath: vi.fn(),
      roundRect: vi.fn(),
      fill: vi.fn(),
      stroke: vi.fn(),
      fillRect: vi.fn(),
      strokeRect: vi.fn(),
      fillText: vi.fn(),
      font: "",
      fillStyle: "",
      strokeStyle: "",
      lineWidth: 0,
      textBaseline: "",
    } as unknown as CanvasRenderingContext2D;
    const drawn = "data:image/png;base64,AAAA";
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(ctx);
    vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue(drawn);

    expect(dragPreviewIcon("notes.txt")).toBe(drawn);
    // A long name is elided rather than drawn off the edge of the preview.
    expect(dragPreviewIcon("a-really-quite-long-file-name-indeed.txt")).toBe(drawn);
    expect(ctx.fillText).toHaveBeenLastCalledWith(
      expect.stringContaining("…"),
      expect.any(Number),
      expect.any(Number),
    );
  });
});
