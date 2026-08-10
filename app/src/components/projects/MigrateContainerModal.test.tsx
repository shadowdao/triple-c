import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import MigrateContainerModal from "./MigrateContainerModal";
import type { ContainerMigration } from "../../hooks/useContainerMigration";
import type { ContainerStaleness } from "../../lib/types";

/** Modal focuses via rAF so the panel is laid out first; jsdom needs a flush. */
async function flushFocus() {
  await act(async () => {
    vi.advanceTimersByTime(20);
  });
}

const STALE: ContainerStaleness = {
  stale: true,
  known: true,
  base_image_id: "sha256:aaa",
  current_base_image_id: "sha256:bbb",
  snapshot_created_at: "2026-03-01T09:00:00Z",
  missing_paths: ["/usr/bin/socat"],
  missing_features: ["Auth bridge tunnel (socat)", "Mission Control"],
  apt_delta: ["socat", "bubblewrap"],
  npm_global_delta: [],
  verbatim_paths: [],
  unpreserved_data: [],
  outdated_package_count: 61,
  probe_error: null,
};

function migration(overrides: Partial<ContainerMigration> = {}): ContainerMigration {
  return {
    staleness: STALE,
    probing: false,
    probeSettled: true,
    running: false,
    recovered: false,
    interrupted: null,
    report: null,
    log: [],
    phaseMessage: null,
    busy: false,
    start: vi.fn(async () => {}),
    resume: vi.fn(async () => {}),
    keep: vi.fn(async () => {}),
    rollback: vi.fn(async () => {}),
    dismiss: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
    ...overrides,
  };
}

async function renderModal(
  staleness: ContainerStaleness | null = STALE,
  overrides: Partial<ContainerMigration> = {},
) {
  const m = migration({ staleness, ...overrides });
  const onClose = vi.fn();
  render(
    <MigrateContainerModal
      projectName="api-server"
      staleness={staleness}
      migration={m}
      onClose={onClose}
    />,
  );
  await flushFocus();
  return { m, onClose };
}

describe("MigrateContainerModal", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "setTimeout"] });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  describe("pre-flight", () => {
    it("leads with what is kept, as a statement rather than a choice", async () => {
      await renderModal();
      const kept = screen.getByText("Kept automatically");
      expect(kept).toBeInTheDocument();
      expect(screen.getByText(/no signing in again/i)).toBeInTheDocument();
      expect(screen.getByText(/every saved session transcript/i)).toBeInTheDocument();
      expect(screen.getByText(/are Docker volumes/i)).toBeInTheDocument();

      // Reassurance comes first: it is above the replay section in the DOM.
      const replay = screen.getByText(/Reinstalled from the new base's repos/);
      expect(kept.compareDocumentPosition(replay)).toBe(
        Node.DOCUMENT_POSITION_FOLLOWING,
      );

      // And it is a statement — there is no switch attached to it.
      const keptSection = kept.closest("section");
      expect(keptSection?.querySelector('[role="switch"]')).toBeNull();
    });

    it("hides the verbatim-copy section when nothing user-authored was found", async () => {
      await renderModal({ ...STALE, verbatim_paths: [] });
      expect(screen.queryByText(/Copied across as-is/i)).not.toBeInTheDocument();
    });

    it("shows the verbatim-copy section with its paths when there are some", async () => {
      await renderModal({
        ...STALE,
        verbatim_paths: ["/usr/local/bin/deploy.sh", "/etc/pki/corp.crt"],
      });
      expect(screen.getByText("Copied across as-is (2)")).toBeInTheDocument();
      expect(screen.getByText("/usr/local/bin/deploy.sh")).toBeInTheDocument();
      expect(screen.getByText("/etc/pki/corp.crt")).toBeInTheDocument();
    });

    it("counts the apt packages and states the rollback's disk cost", async () => {
      await renderModal();
      expect(
        screen.getByText("Reinstalled from the new base's repos (2)"),
      ).toBeInTheDocument();
      expect(screen.getByText("socat")).toBeInTheDocument();
      expect(screen.getByText("bubblewrap")).toBeInTheDocument();
      expect(screen.getByText(/3.8–12.3 GB/)).toBeInTheDocument();
      expect(
        screen.getByText(/Rollback restores the system layer only/i),
      ).toBeInTheDocument();
    });

    it("lists the gains as the inverse of the missing features", async () => {
      await renderModal();
      expect(screen.getByText("You will gain")).toBeInTheDocument();
      expect(screen.getByText(/Auth bridge tunnel \(socat\)/)).toBeInTheDocument();
      expect(screen.getByText(/Mission Control/)).toBeInTheDocument();
      expect(
        screen.getByText(
          /61 packages the current base carries at a different version/i,
        ),
      ).toBeInTheDocument();
    });

    it("passes the three options through when the run is started", async () => {
      const { m } = await renderModal({
        ...STALE,
        verbatim_paths: ["/usr/local/bin/deploy.sh"],
      });
      fireEvent.click(
        screen.getByRole("switch", {
          name: /Keep a rollback image until I confirm/i,
        }),
      );
      fireEvent.click(
        screen.getByRole("button", { name: "Update container base" }),
      );
      expect(m.start).toHaveBeenCalledWith({
        replay_packages: true,
        copy_paths: true,
        keep_rollback: false,
      });
    });

    it("never derives copy_paths from a delta the probe may not have read", async () => {
      // The regression: `copy_paths: copyPaths && verbatim.length > 0` read the
      // toggle's meaning off `staleness`, which is null while the ~6 s probe
      // runs. That sent `copy_paths: false` to a backend that recomputes the
      // real set but honours the flag — files silently not copied, while this
      // dialog said there was nothing to copy. The toggle's own value is the
      // only thing that may be sent; the backend skips the step when *its* set
      // comes out empty, which is the only place that knows.
      const { m } = await renderModal({ ...STALE, verbatim_paths: [] });
      fireEvent.click(
        screen.getByRole("button", { name: "Update container base" }),
      );
      expect(m.start).toHaveBeenCalledWith({
        replay_packages: true,
        copy_paths: true,
        keep_rollback: true,
      });
    });

    it("cannot be started until the probe has settled, and says so", async () => {
      await renderModal(null, { probeSettled: false, probing: true });
      expect(
        screen.getByRole("button", { name: "Update container base" }),
      ).toBeDisabled();
      expect(
        screen.getByText(/lists below are not complete until it finishes/i),
      ).toBeInTheDocument();
      // "None found" and "not checked yet" must not be the same sentence.
      expect(
        screen.getByText(/Still checking which apt packages/i),
      ).toBeInTheDocument();
      expect(screen.getByText("Not checked yet.")).toBeInTheDocument();
      expect(
        screen.queryByText(/No extra apt packages were found/i),
      ).not.toBeInTheDocument();
    });

    it("names the data under /var that the update destroys and cannot restore", async () => {
      await renderModal({
        ...STALE,
        unpreserved_data: [
          { path: "/var/lib/postgresql", bytes: 41_000_000, file_count: 912 },
        ],
      });
      const panel = screen.getByTestId("migration-unpreserved");
      expect(panel.textContent).toMatch(/\/var\/lib\/postgresql/);
      expect(panel.textContent).toMatch(/41\.0 MB in 912 files/);
      expect(panel.textContent).toMatch(/reinstalling the package does not bring it back/i);
    });

    it("says plainly that /var is not carried across even when nothing is at risk", async () => {
      await renderModal();
      const panel = screen.getByTestId("migration-unpreserved");
      expect(panel.textContent).toMatch(/nothing here to lose/i);
      expect(panel.textContent).toMatch(/Data written under \/var is not carried across/i);
    });

    it("offers Resume rather than Keep on a container that is mid-swap", async () => {
      // Keep drops the rollback image, and on an unfinished migration
      // `:latest` still points at the old lineage — so Keep here deletes the
      // only way back from a container the app can no longer reason about.
      const { m } = await renderModal(STALE, {
        interrupted: {
          phase: "interrupted",
          from_image_id: "sha256:aaa",
          to_base_id: "sha256:bbb",
          started_at: "2026-08-09T10:00:00Z",
          report: null,
          rollback_image: "triple-c-snapshot-p1:pre-migration-20260809-100000",
          staging_path: null,
          options: { replay_packages: true, copy_paths: true, keep_rollback: true },
          plan: null,
        },
        report: {
          phase: "failed",
          packages_requested: [],
          packages_installed: [],
          packages_failed: [],
          paths_copied: [],
          features_restored: [],
          rollback_available: true,
          message: "saving it failed",
        },
      });
      expect(screen.queryByRole("button", { name: "Keep" })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Resume update" }));
      expect(m.resume).toHaveBeenCalledTimes(1);
    });

    it("does not start anything on cancel", async () => {
      const { m, onClose } = await renderModal();
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(m.start).not.toHaveBeenCalled();
    });
  });

  describe("mid-run", () => {
    const RUNNING: Partial<ContainerMigration> = {
      running: true,
      log: ["Snapshotting container…", "Creating container on the new base…"],
      phaseMessage: "Creating container on the new base…",
    };

    it("streams the phase message and the output", async () => {
      await renderModal(STALE, RUNNING);
      expect(screen.getByRole("status").textContent).toBe(
        "Creating container on the new base…",
      );
      const log = screen.getByTestId("migration-log");
      expect(log.textContent).toContain("Snapshotting container…");
      expect(log.textContent).toContain("Creating container on the new base…");
    });

    it("can be dismissed without cancelling the run", async () => {
      const { m, onClose } = await renderModal(STALE, RUNNING);
      // A run takes minutes; blocking the app for it would be wrong, so the
      // dialog closes and the work carries on.
      expect(
        screen.getByText(/keeps running if you close it/i),
      ).toBeInTheDocument();

      fireEvent.click(screen.getByRole("button", { name: "Hide" }));
      expect(onClose).toHaveBeenCalledTimes(1);

      // Nothing on the migration was touched — closing is not cancelling.
      expect(m.start).not.toHaveBeenCalled();
      expect(m.rollback).not.toHaveBeenCalled();
      expect(m.dismiss).not.toHaveBeenCalled();
    });

    it("still closes on Escape and on the header ✕ while running", async () => {
      const { m, onClose } = await renderModal(STALE, RUNNING);
      fireEvent.click(screen.getByRole("button", { name: "Close dialog" }));
      expect(onClose).toHaveBeenCalledTimes(1);
      fireEvent.keyDown(document, { key: "Escape" });
      expect(onClose).toHaveBeenCalledTimes(2);
      expect(m.dismiss).not.toHaveBeenCalled();
    });
  });

  describe("outcome", () => {
    it("shows the report in place of the pre-flight once it lands", async () => {
      await renderModal(STALE, {
        report: {
          phase: "partial",
          packages_requested: ["socat", "bubblewrap"],
          packages_installed: ["socat"],
          packages_failed: [
            { name: "bubblewrap", reason: "held back by apt-mark" },
          ],
          paths_copied: [],
          features_restored: ["Auth bridge tunnel (socat)"],
          rollback_available: true,
          message: "",
        },
      });
      expect(screen.getByText(/Updated, but not completely/i)).toBeInTheDocument();
      expect(screen.queryByText("Kept automatically")).not.toBeInTheDocument();
      expect(screen.getByText(/held back by apt-mark/)).toBeInTheDocument();
    });
  });
});
