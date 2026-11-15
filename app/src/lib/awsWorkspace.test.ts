import { describe, it, expect } from "vitest";
import {
  cleanRepos,
  environmentSelectable,
  memoryError,
  memoryOptions,
  nearestMemory,
  repoFolderFromUrl,
  stopGuardNeeded,
} from "./awsWorkspace";
import { environment } from "../test/awsFixtures";

describe("awsWorkspace", () => {
  it("knows Fargate's valid CPU/memory pairs", () => {
    expect(memoryOptions(256)).toEqual([512, 1024, 1536, 2048]);
    expect(memoryError(1024, 4096)).toBeNull();
    expect(memoryError(256, 4096)).toMatch(/not valid with 256/);
    expect(memoryError(8192, 17000)).toMatch(/steps of 4096/);
    expect(memoryError(300, 1024)).toMatch(/not a Fargate CPU/);
    expect(nearestMemory(256, 4096)).toBe(2048);
    expect(nearestMemory(16384, 4096)).toBe(32768);
  });

  it("derives a clone folder from a URL", () => {
    expect(repoFolderFromUrl("https://github.com/org/api.git")).toBe("api");
    expect(repoFolderFromUrl("git@github.com:org/web-app.git")).toBe("web-app");
    expect(repoFolderFromUrl("ssh://h/x/y z/")).toBe("y-z");
  });

  it("drops blank repository rows and keeps an empty branch as the default", () => {
    expect(
      cleanRepos([
        { url: " https://h/r.git ", branch: " ", folder: "" },
        { url: "", branch: null, folder: "" },
      ]),
    ).toEqual([{ url: "https://h/r.git", branch: null, folder: "r" }]);
  });

  it("guards a stop with unsaved or unknown work", () => {
    const repo = { folder: "a", branch: "main", dirty: false, unpushed: 0 };
    expect(stopGuardNeeded({ checked: true, repos: [repo], error: null })).toBe(false);
    expect(stopGuardNeeded({ checked: true, repos: [{ ...repo, dirty: true }], error: null })).toBe(true);
    expect(stopGuardNeeded({ checked: true, repos: [{ ...repo, unpushed: 1 }], error: null })).toBe(true);
    expect(stopGuardNeeded({ checked: false, repos: [], error: "x" })).toBe(true);
  });

  it("selects only ready environments the user joined", () => {
    expect(environmentSelectable(environment("a", "ready", true))).toBe(true);
    expect(environmentSelectable(environment("a", "update_available", true))).toBe(true);
    expect(environmentSelectable(environment("a", "ready", false))).toBe(false);
    expect(environmentSelectable(environment("a", "deploying", true))).toBe(false);
  });
});
