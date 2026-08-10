import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useContainerMigration } from "./useContainerMigration";
import type {
  ContainerStaleness,
  MigrationReport,
  MigrationState,
  Project,
} from "../lib/types";

const getContainerStaleness = vi.fn();
const getMigrationState = vi.fn();
const migrateProjectToBase = vi.fn();
const confirmMigration = vi.fn();
const rollbackMigration = vi.fn();
const pushToast = vi.fn();
let progress: string | undefined;

vi.mock("../lib/tauri-commands", () => ({
  getContainerStaleness: (...a: unknown[]) => getContainerStaleness(...a),
  getMigrationState: (...a: unknown[]) => getMigrationState(...a),
  migrateProjectToBase: (...a: unknown[]) => migrateProjectToBase(...a),
  confirmMigration: (...a: unknown[]) => confirmMigration(...a),
  rollbackMigration: (...a: unknown[]) => rollbackMigration(...a),
}));

vi.mock("../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) =>
      selector({ pushToast, containerProgress: { p1: progress } }),
    {
      getState: () => ({ setContainerProgress: () => {} }),
    },
  ),
}));

const STALE: ContainerStaleness = {
  stale: true,
  known: true,
  base_image_id: "sha256:aaa",
  current_base_image_id: "sha256:bbb",
  snapshot_created_at: "2026-03-01T09:00:00Z",
  missing_paths: ["/usr/bin/socat"],
  missing_features: ["Auth bridge tunnel (socat)"],
  apt_delta: ["socat"],
  npm_global_delta: [],
  verbatim_paths: [],
  outdated_package_count: 61,
  probe_error: null,
};

const FRESH: ContainerStaleness = {
  ...STALE,
  stale: false,
  base_image_id: "sha256:bbb",
  missing_paths: [],
  missing_features: [],
  apt_delta: [],
  outdated_package_count: 0,
};

const CLEAN: MigrationReport = {
  phase: "succeeded",
  packages_requested: ["socat"],
  packages_installed: ["socat"],
  packages_failed: [],
  paths_copied: [],
  features_restored: ["Auth bridge tunnel (socat)"],
  rollback_available: true,
  message: "",
};

const OPTIONS = {
  replay_packages: true,
  copy_paths: false,
  keep_rollback: true,
};

function state(overrides: Partial<MigrationState> = {}): MigrationState {
  return {
    phase: "in-progress",
    from_image_id: "sha256:aaa",
    to_base_id: "sha256:bbb",
    started_at: "2026-08-09T10:00:00Z",
    report: null,
    rollback_image: "triple-c-snapshot-p1:pre-migration-1754733600",
    staging_path: null,
    options: OPTIONS,
    plan: null,
    ...overrides,
  };
}

const project = { id: "p1", name: "api-server", container_id: "c1", status: "stopped" } as Project;

describe("useContainerMigration", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    progress = undefined;
    getContainerStaleness.mockResolvedValue(STALE);
    getMigrationState.mockResolvedValue(null);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("probes staleness for a container that exists", async () => {
    const { result } = renderHook(() => useContainerMigration(project));
    await waitFor(() => expect(result.current.staleness).toEqual(STALE));
    expect(getContainerStaleness).toHaveBeenCalledWith("p1");
  });

  it("does not probe a project whose container was never created", async () => {
    renderHook(() =>
      useContainerMigration({ ...project, container_id: null } as Project),
    );
    await waitFor(() => expect(getMigrationState).toHaveBeenCalled());
    expect(getContainerStaleness).not.toHaveBeenCalled();
  });

  it("shows an absent banner rather than an error one when the probe fails", async () => {
    getContainerStaleness.mockRejectedValue(new Error("no such container"));
    const { result } = renderHook(() => useContainerMigration(project));
    await waitFor(() => expect(result.current.probing).toBe(false));
    expect(result.current.staleness).toBeNull();
  });

  it("passes the options through and keeps the report", async () => {
    migrateProjectToBase.mockResolvedValue(CLEAN);
    const { result } = renderHook(() => useContainerMigration(project));
    await waitFor(() => expect(result.current.staleness).toEqual(STALE));

    getContainerStaleness.mockResolvedValue(FRESH);
    await act(async () => {
      await result.current.start({
        replay_packages: true,
        copy_paths: false,
        keep_rollback: true,
      });
    });

    expect(migrateProjectToBase).toHaveBeenCalledWith("p1", {
      replay_packages: true,
      copy_paths: false,
      keep_rollback: true,
    });
    expect(result.current.report).toEqual(CLEAN);
    expect(result.current.running).toBe(false);
  });

  it("turns a rejected migrate call into a failed report, not a silent nothing", async () => {
    migrateProjectToBase.mockRejectedValue(new Error("docker daemon went away"));
    const { result } = renderHook(() => useContainerMigration(project));
    await act(async () => {
      await result.current.start({
        replay_packages: true,
        copy_paths: false,
        keep_rollback: true,
      });
    });
    expect(result.current.report?.phase).toBe("failed");
    expect(result.current.report?.message).toMatch(/docker daemon went away/);
    expect(result.current.report?.rollback_available).toBe(false);
  });

  it("clears the report and re-probes once the migration is kept", async () => {
    migrateProjectToBase.mockResolvedValue(CLEAN);
    confirmMigration.mockResolvedValue(undefined);
    const { result } = renderHook(() => useContainerMigration(project));
    await act(async () => {
      await result.current.start({
        replay_packages: true,
        copy_paths: false,
        keep_rollback: true,
      });
    });
    getContainerStaleness.mockResolvedValue(FRESH);
    await act(async () => {
      await result.current.keep();
    });
    expect(confirmMigration).toHaveBeenCalledWith("p1");
    expect(result.current.report).toBeNull();
    await waitFor(() => expect(result.current.staleness).toEqual(FRESH));
  });

  it("says out loud that a rollback left the volumes alone", async () => {
    rollbackMigration.mockResolvedValue(undefined);
    const { result } = renderHook(() => useContainerMigration(project));
    await act(async () => {
      await result.current.rollback();
    });
    expect(rollbackMigration).toHaveBeenCalledWith("p1");
    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "success",
        detail: expect.stringMatching(/Volumes were not touched/i),
      }),
    );
  });

  describe("crash recovery", () => {
    it("adopts a run that was still in progress, and polls it to a report", async () => {
      getMigrationState.mockResolvedValue(state());

      const { result } = renderHook(() => useContainerMigration(project));
      await waitFor(() => expect(result.current.running).toBe(true));
      expect(result.current.recovered).toBe(true);

      getMigrationState.mockResolvedValue(
        state({ phase: "awaiting-confirmation", report: CLEAN }),
      );
      await waitFor(() => expect(result.current.report).toEqual(CLEAN), {
        timeout: 5000,
      });
      expect(result.current.running).toBe(false);
    });

    it("surfaces a finished migration that was never acknowledged", async () => {
      getMigrationState.mockResolvedValue(
        state({ phase: "awaiting-confirmation", report: CLEAN }),
      );
      const { result } = renderHook(() => useContainerMigration(project));
      await waitFor(() => expect(result.current.report).toEqual(CLEAN));
      expect(result.current.running).toBe(false);
    });

    it("surfaces an interrupted migration instead of leaving it invisible", async () => {
      getMigrationState.mockResolvedValue(state({ phase: "interrupted" }));
      const { result } = renderHook(() => useContainerMigration(project));
      await waitFor(() => expect(result.current.interrupted).not.toBeNull());
      // Nothing is driving it, so it is not "running" and has no report.
      expect(result.current.running).toBe(false);
      expect(result.current.report).toBeNull();
    });

    it("resumes an interrupted migration with the options it was given", async () => {
      getMigrationState.mockResolvedValue(state({ phase: "interrupted" }));
      migrateProjectToBase.mockResolvedValue(CLEAN);
      const { result } = renderHook(() => useContainerMigration(project));
      await waitFor(() => expect(result.current.interrupted).not.toBeNull());

      await act(async () => {
        await result.current.resume();
      });
      // The deltas cannot be recomputed after the swap, so the recorded plan's
      // options are replayed verbatim rather than re-derived.
      expect(migrateProjectToBase).toHaveBeenCalledWith("p1", OPTIONS);
      expect(result.current.interrupted).toBeNull();
      expect(result.current.report).toEqual(CLEAN);
    });

    it("ignores an unrecognised phase from a future build rather than crashing", async () => {
      getMigrationState.mockResolvedValue(state({ phase: "quantum-tunnelling" }));
      const { result } = renderHook(() => useContainerMigration(project));
      await waitFor(() => expect(result.current.staleness).toEqual(STALE));
      expect(result.current.running).toBe(false);
      expect(result.current.interrupted).toBeNull();
      expect(result.current.report).toBeNull();
    });
  });
});
