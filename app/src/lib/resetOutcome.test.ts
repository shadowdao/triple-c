import { describe, it, expect } from "vitest";
import { describeResetLeftovers, resetLeftoverPronoun } from "./resetOutcome";
import type { ProjectResetOutcome } from "./types";

function outcome(overrides: Partial<ProjectResetOutcome> = {}): ProjectResetOutcome {
  return {
    project: {} as ProjectResetOutcome["project"],
    leftover_image: null,
    leftover_volumes: [],
    ...overrides,
  };
}

describe("describeResetLeftovers", () => {
  it("names the image first, then the volumes", () => {
    expect(describeResetLeftovers(outcome({ leftover_image: "x" }))).toBe(
      "its previous container image",
    );
    expect(describeResetLeftovers(outcome({ leftover_volumes: ["v1"] }))).toBe("a volume");
    expect(describeResetLeftovers(outcome({ leftover_volumes: ["v1", "v2"] }))).toBe("2 volumes");
    expect(
      describeResetLeftovers(outcome({ leftover_image: "x", leftover_volumes: ["v1", "v2"] })),
    ).toBe("its previous container image and 2 volumes");
  });
});

describe("resetLeftoverPronoun", () => {
  it("is singular for exactly one leftover", () => {
    expect(resetLeftoverPronoun(outcome({ leftover_image: "x" }))).toBe("it");
    expect(resetLeftoverPronoun(outcome({ leftover_volumes: ["v1"] }))).toBe("it");
  });

  it("is plural once more than one thing survived", () => {
    expect(resetLeftoverPronoun(outcome({ leftover_image: "x", leftover_volumes: ["v1"] }))).toBe(
      "them",
    );
    expect(resetLeftoverPronoun(outcome({ leftover_volumes: ["v1", "v2"] }))).toBe("them");
  });
});
