import { describe, it, expect, vi } from "vitest";
import { reattachTab, reattachWithRetry, terminalLabel, terminalsToReopen } from "./persistentTerminals";
import type { ReattachOutcome, WorkspaceTerminal } from "./types";

const noSleep = () => Promise.resolve();
const reattached: ReattachOutcome = { outcome: "reattached", kind: "claude", name: null };

const term = (id: string, known: boolean, kind: "claude" | "bash" = "bash", name: string | null = null): WorkspaceTerminal => ({
  session_id: id,
  kind,
  name,
  created_unix: 1,
  attached: false,
  known,
});

describe("reattachWithRetry", () => {
  it("reattaches at once when the workspace answers", async () => {
    const reattach = vi.fn(async () => reattached);
    expect(await reattachWithRetry({ reattach, stillWanted: () => true, sleep: noSleep })).toBe("reattached");
    expect(reattach).toHaveBeenCalledTimes(1);
  });

  it("stops at once when the session is gone, without retrying", async () => {
    const reattach = vi.fn(async (): Promise<ReattachOutcome> => ({ outcome: "ended" }));
    expect(await reattachWithRetry({ reattach, stillWanted: () => true, sleep: noSleep })).toBe("ended");
    expect(reattach).toHaveBeenCalledTimes(1);
  });

  it("retries while the workspace is unreachable, saying so, then reattaches", async () => {
    const reattach = vi
      .fn<() => Promise<ReattachOutcome>>()
      .mockRejectedValueOnce("not connected")
      .mockRejectedValueOnce("not connected")
      .mockResolvedValue(reattached);
    const onRetrying = vi.fn();
    const sleep = vi.fn(noSleep);
    const r = await reattachWithRetry({ reattach, stillWanted: () => true, onRetrying, sleep, delayMs: 7 });
    expect(r).toBe("reattached");
    expect(onRetrying).toHaveBeenCalledTimes(2);
    expect(sleep).toHaveBeenCalledWith(7);
  });

  it("gives up after its attempts, and is cancelled when the tab or project stops wanting it", async () => {
    const failing = vi.fn(async (): Promise<ReattachOutcome> => {
      throw new Error("down");
    });
    expect(await reattachWithRetry({ reattach: failing, stillWanted: () => true, sleep: noSleep, attempts: 3 })).toBe("gave_up");
    expect(failing).toHaveBeenCalledTimes(3);

    let wanted = true;
    const stopping = vi.fn(async (): Promise<ReattachOutcome> => {
      wanted = false; // the project stopped meanwhile
      throw new Error("not running");
    });
    expect(await reattachWithRetry({ reattach: stopping, stillWanted: () => wanted, sleep: noSleep })).toBe("cancelled");
    expect(stopping).toHaveBeenCalledTimes(1);
  });
});

describe("which terminals come back as tabs", () => {
  it("reopens only this app's terminals that are not open, in order", () => {
    const running = [term("a", true), term("b", false), term("c", true), term("d", true)];
    expect(terminalsToReopen(running, new Set(["c"])).map((t) => t.session_id)).toEqual(["a", "d"]);
  });

  it("makes a reattaching persistent tab with the workspace's kind and label", () => {
    expect(reattachTab("p1", "api", term("k", true, "claude", "main"))).toEqual({
      id: "k",
      projectId: "p1",
      projectName: "api",
      sessionType: "claude",
      sessionName: "main",
      persistent: true,
      pendingReattach: true,
    });
    expect(terminalLabel(term("k", true, "claude"))).toBe("Claude Code");
    expect(terminalLabel(term("k", true, "bash"))).toBe("Shell");
  });
});
