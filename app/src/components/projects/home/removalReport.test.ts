import { describe, it, expect } from "vitest";
import { describeLeftovers, leftoverVerb } from "./removalReport";
import { projectRemovalIsClean } from "../../../lib/types";
import type { ProjectRemovalReport } from "../../../lib/types";

function report(overrides: Partial<ProjectRemovalReport> = {}): ProjectRemovalReport {
  return {
    container: null,
    image: null,
    volumes: [],
    retry_scheduled: false,
    ...overrides,
  };
}

describe("projectRemovalIsClean", () => {
  it("is true only when nothing survived", () => {
    expect(projectRemovalIsClean(report())).toBe(true);
    expect(projectRemovalIsClean(report({ container: "triple-c-abc" }))).toBe(false);
    expect(projectRemovalIsClean(report({ image: "triple-c-snapshot-abc:latest" }))).toBe(false);
    expect(projectRemovalIsClean(report({ volumes: ["triple-c-home-abc"] }))).toBe(false);
  });
});

describe("describeLeftovers", () => {
  it("names each kind of leftover", () => {
    expect(describeLeftovers(report({ container: "triple-c-abc" }))).toBe("its container");
    expect(describeLeftovers(report({ image: "x" }))).toBe("its saved image");
    expect(describeLeftovers(report({ volumes: ["v1"] }))).toBe("a volume");
    expect(describeLeftovers(report({ volumes: ["v1", "v2"] }))).toBe("2 volumes");
  });

  it("joins multiple kinds together", () => {
    expect(
      describeLeftovers(report({ container: "triple-c-abc", image: "x", volumes: ["v1", "v2"] })),
    ).toBe("its container, its saved image, 2 volumes");
  });
});

describe("leftoverVerb", () => {
  it("is singular for exactly one leftover of any kind", () => {
    expect(leftoverVerb(report({ container: "triple-c-abc" }))).toBe("was");
    expect(leftoverVerb(report({ image: "x" }))).toBe("was");
    expect(leftoverVerb(report({ volumes: ["v1"] }))).toBe("was");
  });

  it("is plural once more than one thing survived, including multiple volumes alone", () => {
    expect(leftoverVerb(report({ container: "triple-c-abc", image: "x" }))).toBe("were");
    expect(leftoverVerb(report({ volumes: ["v1", "v2"] }))).toBe("were");
  });
});
