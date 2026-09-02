import { describe, it, expect } from "vitest";
import { sessionDisplayName } from "./sessionName";
import type { Project, TerminalSession } from "./types";

const session = (over: Partial<TerminalSession> = {}): TerminalSession => ({
  id: "s1",
  projectId: "p1",
  projectName: "api",
  sessionType: "claude",
  sessionName: null,
  ...over,
});

const project = (renamed: Record<string, string> = {}) =>
  ({ id: "p1", name: "api", renamed_session_names: renamed }) as unknown as Project;

describe("sessionDisplayName", () => {
  it("prefers a user-set custom name, prefixed with the project", () => {
    expect(sessionDisplayName(session(), project({ s1: "release work" }))).toBe(
      "api: release work",
    );
  });

  it("falls back to the session name when there is no custom one", () => {
    expect(sessionDisplayName(session({ sessionName: "review" }), project())).toBe("review");
  });

  it("falls back to the project name when there is no session name", () => {
    expect(sessionDisplayName(session(), project())).toBe("api");
  });

  it("marks bash sessions", () => {
    expect(sessionDisplayName(session({ sessionType: "bash" }), project())).toBe("api (bash)");
  });

  it("works with no project, which is how a closing tab renders", () => {
    expect(sessionDisplayName(session())).toBe("api");
  });

  it("does not mark bash when a custom name is set, matching the existing rule", () => {
    expect(
      sessionDisplayName(session({ sessionType: "bash" }), project({ s1: "logs" })),
    ).toBe("api: logs");
  });
});
