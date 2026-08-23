import { describe, it, expect } from "vitest";
import { formatBytes, formatBytesCeiling, formatBytesDelta } from "./formatBytes";

describe("formatBytes", () => {
  it("defaults to base 1000, because that is what Docker prints", () => {
    // The Disk panel exists to explain `docker system df`, which formats with
    // `units.HumanSize` — base 1000. Showing 26.1 GB against a terminal saying
    // 28.0 GB for the same build cache reads as a bug in the panel.
    expect(formatBytes(28_000_000_000)).toBe("28.0 GB");
    expect(formatBytes(1_000)).toBe("1.0 KB");
    expect(formatBytes(1_500_000)).toBe("1.5 MB");
    expect(formatBytes(12_273_392_374)).toBe("12.3 GB");
  });

  it("leaves whole bytes without a decimal point", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(999)).toBe("999 B");
  });

  it("reproduces the Project Home convention exactly under `binary`", () => {
    // Three modules import `projects/home/format.ts#formatBytes`, which is now
    // this function. Its output had to be byte-identical or re-pointing it
    // would have quietly changed every file listing in the app.
    expect(formatBytes(1023, { binary: true })).toBe("1023 B");
    expect(formatBytes(1024, { binary: true })).toBe("1.0 KB");
    expect(formatBytes(1024 * 1024, { binary: true })).toBe("1.0 MB");
    expect(formatBytes(1024 * 1024 * 1024, { binary: true })).toBe("1.0 GB");
    expect(formatBytes(1_610_612_736, { binary: true })).toBe("1.5 GB");
  });

  it("reproduces the migration convention exactly by default", () => {
    // `migrationCopy.formatDataSize` is now a call to this, and its output is
    // asserted in MigrateContainerModal.test.tsx.
    expect(formatBytes(41_000_000)).toBe("41.0 MB");
    expect(formatBytes(3_800_000_000)).toBe("3.8 GB");
  });

  it("labels binary units honestly when asked to", () => {
    expect(formatBytes(1024, { binary: true, iec: true })).toBe("1.0 KiB");
    expect(formatBytes(1024 ** 3, { binary: true, iec: true })).toBe("1.0 GiB");
  });

  it("climbs to TB rather than showing five-digit gigabytes", () => {
    expect(formatBytes(2_500_000_000_000)).toBe("2.5 TB");
  });

  it("renders an em dash for a size the daemon did not compute", () => {
    // Docker reports -1 for "not calculated" on shared sizes and volume ref
    // counts. `NaN GB` in the middle of a table is worse than nothing.
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(NaN)).toBe("—");
    expect(formatBytes(Infinity)).toBe("—");
  });

  it("honours a requested precision", () => {
    expect(formatBytes(1_234_567_890, { precision: 2 })).toBe("1.23 GB");
    expect(formatBytes(1_234_567_890, { precision: 0 })).toBe("1 GB");
  });
});

describe("formatBytesDelta", () => {
  it("signs a figure that is being added rather than measured", () => {
    // "Next commit adds +868.0 MB" — the sign is what makes it read as a cost
    // about to be incurred rather than a size already on disk.
    expect(formatBytesDelta(868_000_000)).toBe("+868.0 MB");
    expect(formatBytesDelta(0)).toBe("+0 B");
  });

  it("does not sign an unknown", () => {
    expect(formatBytesDelta(-1)).toBe("—");
  });
});

describe("formatBytesCeiling", () => {
  it("says 'up to', because a compaction's yield is a bound not a promise", () => {
    // Every other figure in the Disk panel is measured. This one cannot be
    // known until the rewrite runs, and rendering it through a separate
    // function is what stops it being read as a guarantee.
    expect(formatBytesCeiling(5_100_000_000)).toBe("up to 5.1 GB");
  });

  it("refuses to imply a saving when there is no bound to give", () => {
    expect(formatBytesCeiling(0)).toBe("an unknown amount");
    expect(formatBytesCeiling(-1)).toBe("an unknown amount");
  });
});
