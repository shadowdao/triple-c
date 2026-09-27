import { describe, it, expect } from "vitest";
import {
  effectiveInstalls,
  formatItemRef,
  isStale,
  itemRefKey,
  projectItemState,
  STALE_AFTER_MS,
} from "./marketplace";
import type { MarketplaceInstall, MarketplaceSnapshot, Project } from "./types";

const A = "a".repeat(40);
const B = "b".repeat(40);

const inst = (key: string, commit = A, kind: MarketplaceInstall["kind"] = "agent"): MarketplaceInstall => ({
  marketplace_id: "m1",
  kind,
  key,
  commit,
});

const project = (patch: Partial<Project> = {}): Project =>
  ({
    id: "p1",
    name: "api",
    marketplace_installs: [],
    marketplace_disabled: [],
    ...patch,
  }) as unknown as Project;

describe("itemRefKey / formatItemRef", () => {
  it("keys and formats a ref", () => {
    const r = { marketplace_id: "m1", kind: "hook" as const, key: "notify" };
    expect(itemRefKey(r)).toBe("m1/hook/notify");
    expect(formatItemRef(r)).toBe("hook:notify");
  });
});

describe("projectItemState", () => {
  const ref = { marketplace_id: "m1", kind: "agent" as const, key: "rev" };

  it("is none when nothing installs it", () => {
    expect(projectItemState(ref, [], project())).toBe("none");
  });

  it("is inherited from a global install", () => {
    expect(projectItemState(ref, [inst("rev")], project())).toBe("inherited");
  });

  it("is opted_out when the project disabled the global install", () => {
    const p = project({ marketplace_disabled: [ref] });
    expect(projectItemState(ref, [inst("rev")], p)).toBe("opted_out");
  });

  it("is project for a project-only install", () => {
    const p = project({ marketplace_installs: [inst("rev")] });
    expect(projectItemState(ref, [], p)).toBe("project");
  });

  it("is project when project and global share the pin", () => {
    const p = project({ marketplace_installs: [inst("rev", A)] });
    expect(projectItemState(ref, [inst("rev", A)], p)).toBe("project");
  });

  it("flags a project pin that differs from the global pin", () => {
    const p = project({ marketplace_installs: [inst("rev", B)] });
    expect(projectItemState(ref, [inst("rev", A)], p)).toBe("project_pinned_differently");
  });

  it("does not confuse kinds with the same key", () => {
    expect(projectItemState(ref, [inst("rev", A, "skill")], project())).toBe("none");
  });
});

describe("effectiveInstalls", () => {
  it("merges global minus disabled plus project, project winning", () => {
    const disabledRef = { marketplace_id: "m1", kind: "agent" as const, key: "off" };
    const p = project({
      marketplace_disabled: [disabledRef],
      marketplace_installs: [inst("both", B), inst("mine")],
    });
    const out = effectiveInstalls([inst("glob"), inst("off"), inst("both", A)], p);
    expect(out.map((i) => [i.key, i.commit, i.source])).toEqual([
      ["both", B, "project"],
      ["glob", A, "global"],
      ["mine", A, "project"],
    ]);
  });
});

describe("isStale", () => {
  const snap = (fetched_at: string | null): MarketplaceSnapshot => ({
    marketplace_id: "m1",
    head_commit: null,
    fetched_at,
    fetch_error: null,
    items: [],
  });
  const now = Date.parse("2026-09-27T12:00:00Z");

  it("treats a never-fetched snapshot as stale", () => {
    expect(isStale(snap(null), now)).toBe(true);
  });

  it("is fresh within 15 minutes and stale after", () => {
    expect(isStale(snap(new Date(now - STALE_AFTER_MS + 1000).toISOString()), now)).toBe(false);
    expect(isStale(snap(new Date(now - STALE_AFTER_MS - 1000).toISOString()), now)).toBe(true);
  });

  it("treats an unparsable timestamp as stale", () => {
    expect(isStale(snap("not a date"), now)).toBe(true);
  });
});
