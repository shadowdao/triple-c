import { describe, expect, it } from "vitest";
import { canSave, initialViewerState, pollEffect, reduceViewer, type ViewerDocState } from "./viewerState";

const H1 = "1".repeat(64);
const H2 = "2".repeat(64);
const H3 = "3".repeat(64);
const loaded = (truncated = false): ViewerDocState =>
  reduceViewer(initialViewerState, { type: "loaded", hash: H1, truncated });
const poll = (s: ViewerDocState, hash: string | null, exists = true) =>
  reduceViewer(s, { type: "polled", poll: { exists, hash, size: exists ? 1 : null } });

describe("reduceViewer", () => {
  it("seeds both hashes from an untruncated load", () => {
    expect(loaded()).toMatchObject({ doc: "clean", disk: "same", baseHash: H1, diskHash: H1 });
  });
  it("leaves diskHash unknown after a truncated load, so the first poll seeds it silently", () => {
    const s = loaded(true);
    expect(s.diskHash).toBeNull();
    const after = poll(s, H2);
    expect(after).toMatchObject({ disk: "same", diskHash: H2 });
    expect(pollEffect(s, after)).toBe("none");
  });
  it("an unchanged poll is a no-op", () => {
    const s = loaded();
    expect(pollEffect(s, poll(s, H1))).toBe("none");
  });
  it("a changed poll on a clean doc reloads", () => {
    const s = loaded();
    const after = poll(s, H2);
    expect(after).toMatchObject({ disk: "changed", diskHash: H2, doc: "clean" });
    expect(pollEffect(s, after)).toBe("reload");
    const reloaded = reduceViewer(after, { type: "reloaded", hash: H2, truncated: false, polledHash: H2 });
    expect(reloaded).toMatchObject({ disk: "same", baseHash: H2, diskHash: H2, justReloaded: true });
  });
  it("a truncated reload adopts the polled hash, not the prefix hash; the next identical poll is a no-op", () => {
    // A truncated load never gets a comparable full-file hash of its own, so a
    // poll-driven reload of a large file must seed diskHash from the poll's
    // hash (spec Decision 2) -- otherwise every poll re-triggers a reload.
    const seeded = poll(loaded(true), H2);
    const changed = poll(seeded, H3);
    expect(pollEffect(seeded, changed)).toBe("reload");
    const reloaded = reduceViewer(changed, { type: "reloaded", hash: H1, truncated: true, polledHash: changed.diskHash });
    expect(reloaded).toMatchObject({ disk: "same", diskHash: H3, baseHash: H1, justReloaded: true });
    expect(pollEffect(reloaded, poll(reloaded, H3))).toBe("none");
  });
  it("a clean doc still marked changed (its reload failed) retries on the next identical poll", () => {
    const changed = poll(loaded(), H2);
    expect(pollEffect(changed, poll(changed, H2))).toBe("reload");
  });
  it("a changed poll on a dirty doc shows the banner and never reloads", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    const after = poll(s, H2);
    expect(after).toMatchObject({ doc: "dirty", disk: "changed" });
    expect(pollEffect(s, after)).toBe("banner");
    expect(pollEffect(after, poll(after, H2))).toBe("none");
  });
  it("overwrite-on-save adopts the disk hash as the base", () => {
    const s = poll(reduceViewer(loaded(), { type: "edited" }), H2);
    const o = reduceViewer(s, { type: "overwrite_on_save" });
    expect(o).toMatchObject({ baseHash: H2, disk: "same", overwrite: true, doc: "dirty" });
    expect(canSave(o, true)).toBe(true);
  });
  it("a save clears dirty and aligns hashes; a conflict marks disk changed", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    expect(reduceViewer(s, { type: "saved", hash: H2, diskHash: H2 })).toMatchObject({ doc: "clean", disk: "same", baseHash: H2, diskHash: H2, overwrite: false });
    expect(reduceViewer(s, { type: "save_conflict" })).toMatchObject({ doc: "dirty", disk: "changed" });
    expect(reduceViewer(s, { type: "save_gone" })).toMatchObject({ disk: "gone" });
  });
  it("a save another writer overtook keeps our base but shows Changed on disk (M2)", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    const raced = reduceViewer(s, { type: "saved", hash: H2, diskHash: H3 });
    expect(raced).toMatchObject({ doc: "dirty", disk: "changed", baseHash: H2, diskHash: H3, overwrite: false });
    expect(canSave(raced, true)).toBe(false);
    // The next poll reporting that same foreign hash is quiet: the banner stays up.
    const next = poll(raced, H3);
    expect(next).toMatchObject({ disk: "changed", doc: "dirty" });
    expect(pollEffect(raced, next)).toBe("none");
    // Overwrite adopts what is on disk, not our own hash.
    expect(reduceViewer(next, { type: "overwrite_on_save" })).toMatchObject({ baseHash: H3, disk: "same" });
  });
  it("a gone file disables saving but keeps the buffer state", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    const gone = poll(s, null, false);
    expect(gone).toMatchObject({ disk: "gone", doc: "dirty" });
    expect(canSave(gone, true)).toBe(false);
    expect(pollEffect(s, gone)).toBe("banner");
  });
  it("a poll refused as not running flags the container down and a good one clears it", () => {
    // Regression: start from a dirty doc, not a clean one -- otherwise
    // canSave(down, true) is false purely because doc !== "dirty", and the
    // assertion never actually exercises containerDown.
    const dirty = reduceViewer(loaded(), { type: "edited" });
    const down = reduceViewer(dirty, {
      type: "poll_failed",
      message: "Start the project before checking this file for changes — it runs inside the running container.",
    });
    expect(down).toMatchObject({ containerDown: true, pollError: null });
    expect(canSave(down, true)).toBe(false);
    expect(poll(down, H1).containerDown).toBe(false);
  });
  it("any other poll failure is kept as its own message, does not claim the container is down, and clears on a good poll", () => {
    const dirty = reduceViewer(loaded(), { type: "edited" });
    const down = reduceViewer(dirty, { type: "poll_failed", message: "Start the project before checking this file for changes — files live in its container." });
    const failed = reduceViewer(down, { type: "poll_failed", message: "Could not check the file: Permission denied" });
    expect(failed).toMatchObject({ containerDown: false, pollError: "Could not check the file: Permission denied" });
    expect(canSave(failed, true)).toBe(true);
    expect(poll(failed, H1)).toMatchObject({ pollError: null, containerDown: false });
    expect(poll(failed, null, false)).toMatchObject({ pollError: null, disk: "gone" });
  });
  it("a hash-less poll and a gone file reappearing both clear the flags; the reappeared file is same", () => {
    const gone = poll(loaded(), null, false);
    expect(poll(gone, H1)).toMatchObject({ disk: "same" });
    expect(poll(gone, null)).toMatchObject({ disk: "gone", containerDown: false, pollError: null });
  });
  it("canSave needs dirty + editable + disk in sync", () => {
    expect(canSave(loaded(), true)).toBe(false);
    const dirty = reduceViewer(loaded(), { type: "edited" });
    expect(canSave(dirty, true)).toBe(true);
    expect(canSave(dirty, false)).toBe(false);
    expect(canSave(poll(dirty, H2), true)).toBe(false);
  });
});
