import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import AuthBridgeRow, { bridgeIndicator } from "./AuthBridgeRow";
import type { AuthBridgeStatus, Project } from "../../../../lib/types";

/**
 * The bridge shipped with a working backend, a typed IPC wrapper, and no way to
 * reach either: `setAuthBridgeEnabled` had zero call sites, and the
 * `auth-bridge-changed` event had no listener — so a host port the bridge could
 * not take was a silent failure that presented as a login that simply hung.
 * These tests hold both halves down.
 */

const getAuthBridgeStatus = vi.fn<() => Promise<AuthBridgeStatus>>();
const setAuthBridgeEnabled = vi.fn<(id: string, on: boolean) => Promise<AuthBridgeStatus>>();

vi.mock("../../../../lib/tauri-commands", () => ({
  getAuthBridgeStatus: () => getAuthBridgeStatus(),
  setAuthBridgeEnabled: (id: string, on: boolean) => setAuthBridgeEnabled(id, on),
}));

/** Captured so a test can push an `auth-bridge-changed` payload by hand. */
let emit: ((payload: unknown) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: (e: { payload: unknown }) => void) => {
    emit = (payload) => handler({ payload });
    return () => {
      emit = null;
    };
  }),
}));

const OFF: AuthBridgeStatus = { enabled: false, active_ports: [], conflicts: [] };

const project = {
  id: "p1",
  name: "api",
  status: "running",
  auth_bridge_enabled: false,
} as unknown as Project;

beforeEach(() => {
  vi.clearAllMocks();
  getAuthBridgeStatus.mockResolvedValue(OFF);
  setAuthBridgeEnabled.mockResolvedValue({ ...OFF, enabled: true });
});

describe("AuthBridgeRow", () => {
  it("turns the bridge on through its own command, not the project save", async () => {
    // The dedicated command exists so this can be flipped while the container
    // runs — which is exactly when a user discovers they need it. Routing it
    // through the Config tab's stopped-only save would make it unreachable at
    // the only moment it matters.
    render(<AuthBridgeRow project={project} />);
    await waitFor(() => expect(getAuthBridgeStatus).toHaveBeenCalled());

    fireEvent.click(screen.getByRole("switch", { name: "Auth bridge" }));

    await waitFor(() =>
      expect(setAuthBridgeEnabled).toHaveBeenCalledWith("p1", true),
    );
  });

  it("stays usable while the container is running", async () => {
    render(<AuthBridgeRow project={project} />);
    await waitFor(() => expect(getAuthBridgeStatus).toHaveBeenCalled());
    expect(screen.getByRole("switch", { name: "Auth bridge" })).not.toBeDisabled();
  });

  it("reports a port conflict the poller emitted", async () => {
    getAuthBridgeStatus.mockResolvedValue({ ...OFF, enabled: true });
    render(<AuthBridgeRow project={project} />);
    await waitFor(() => expect(emit).not.toBeNull());

    emit!({
      project_id: "p1",
      status: {
        enabled: true,
        active_ports: [],
        conflicts: [
          { port: 54545, reason: "Host port 54545 is already in use (…); not bridged." },
        ],
      },
    });

    expect(await screen.findByText(/Port 54545/)).toBeInTheDocument();
    expect(screen.getByText("Port conflict")).toBeInTheDocument();
  });

  it("ignores an event for a different project", async () => {
    getAuthBridgeStatus.mockResolvedValue({ ...OFF, enabled: true });
    render(<AuthBridgeRow project={project} />);
    await waitFor(() => expect(emit).not.toBeNull());

    emit!({
      project_id: "other",
      status: { enabled: true, active_ports: [], conflicts: [{ port: 1, reason: "nope" }] },
    });

    expect(screen.queryByText(/Port 1:/)).not.toBeInTheDocument();
  });

  it("puts the switch back if the command rejects", async () => {
    setAuthBridgeEnabled.mockRejectedValue("Project p1 not found");
    render(<AuthBridgeRow project={project} />);
    await waitFor(() => expect(getAuthBridgeStatus).toHaveBeenCalled());

    fireEvent.click(screen.getByRole("switch", { name: "Auth bridge" }));

    expect(await screen.findByText(/not found/)).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Auth bridge" })).not.toBeChecked();
  });
});

describe("bridgeIndicator", () => {
  // Every branch is a glyph plus a word — status is never colour alone.
  it("says nothing is on when it is off", () => {
    expect(bridgeIndicator(OFF, true)).toEqual({ tone: "off", label: "Off" });
  });

  it("puts a conflict ahead of everything else", () => {
    expect(
      bridgeIndicator(
        {
          enabled: true,
          active_ports: [
            { port: 1, family: "v4", bridged_at: "", ipv6_warning: null },
          ],
          conflicts: [{ port: 2, reason: "taken" }],
        },
        true,
      ).tone,
    ).toBe("error");
  });

  it("flags a port that only took the IPv4 half", () => {
    // Node resolves `localhost` to IPv6 first on Linux, so a v4-only listener
    // is a callback that never arrives in front of a bridge reporting healthy.
    expect(
      bridgeIndicator(
        {
          enabled: true,
          active_ports: [
            { port: 1, family: "v6", bridged_at: "", ipv6_warning: "no ::1" },
          ],
          conflicts: [],
        },
        true,
      ).label,
    ).toBe("IPv4 only");
  });

  it("counts the ports it is holding", () => {
    expect(
      bridgeIndicator(
        {
          enabled: true,
          active_ports: [
            { port: 1, family: "v4", bridged_at: "", ipv6_warning: null },
            { port: 2, family: "v4", bridged_at: "", ipv6_warning: null },
          ],
          conflicts: [],
        },
        true,
      ).label,
    ).toBe("Bridging 2 ports");
  });

  it("says it is waiting when the container is not running", () => {
    // Enabled and holding nothing is normal; enabled with no container is a
    // different thing, and saying so stops it reading as a failure.
    expect(bridgeIndicator({ ...OFF, enabled: true }, false).label).toBe(
      "Waiting for the container",
    );
    expect(bridgeIndicator({ ...OFF, enabled: true }, true).label).toBe("Watching");
  });
});
