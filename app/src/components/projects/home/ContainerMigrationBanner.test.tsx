import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import ContainerMigrationBanner from "./ContainerMigrationBanner";
import type { ContainerMigration } from "../../../hooks/useContainerMigration";
import type {
  ContainerStaleness,
  MigrationReport,
} from "../../../lib/types";

const FRESH: ContainerStaleness = {
  stale: false,
  known: true,
  base_image_id: "sha256:aaa",
  current_base_image_id: "sha256:aaa",
  snapshot_created_at: "2026-03-01T09:00:00Z",
  missing_paths: [],
  missing_features: [],
  apt_delta: [],
  npm_global_delta: [],
  verbatim_paths: [],
  unpreserved_data: [],
  outdated_package_count: 0,
  probe_error: null,
};

const STALE: ContainerStaleness = {
  ...FRESH,
  stale: true,
  current_base_image_id: "sha256:bbb",
  missing_paths: ["/usr/bin/socat", "/usr/bin/bwrap"],
  missing_features: [
    "Host-browser opening",
    "Auth bridge tunnel (socat)",
    "Mission Control",
  ],
  apt_delta: ["socat", "bubblewrap"],
  outdated_package_count: 61,
};

function migration(overrides: Partial<ContainerMigration> = {}): ContainerMigration {
  return {
    staleness: null,
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

function renderBanner(m: ContainerMigration, canMigrate = true) {
  const onOpen = vi.fn();
  const { container } = render(
    <ContainerMigrationBanner migration={m} canMigrate={canMigrate} onOpen={onOpen} />,
  );
  return { onOpen, container };
}

describe("ContainerMigrationBanner", () => {
  it("renders nothing when the container is on the current base", () => {
    const { container } = renderBanner(migration({ staleness: FRESH }));
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing before the probe has returned", () => {
    const { container } = renderBanner(migration({ staleness: null }));
    expect(container).toBeEmptyDOMElement();
  });

  it("leads with the missing features rather than image digests", () => {
    renderBanner(migration({ staleness: STALE }));
    expect(screen.getByText(/Container base is out of date/i)).toBeInTheDocument();
    expect(
      screen.getByText(
        /Host-browser opening, Auth bridge tunnel \(socat\) and Mission Control/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/61 packages differ from the versions on the current base/i),
    ).toBeInTheDocument();
    // Digests are evidence, not the message.
    expect(screen.queryByText(/sha256/)).not.toBeInTheDocument();
  });

  it("does not claim the packages are behind, only that they differ", () => {
    renderBanner(migration({ staleness: STALE }));
    // `outdated_package_count` is a drift measure; the backend explicitly does
    // not promise every one of them is newer.
    expect(screen.queryByText(/behind on security updates/i)).not.toBeInTheDocument();
  });

  it("says the container was probed when there is no base-image label", () => {
    // `stale` is always false when `known` is false — an unknown lineage is not
    // a claim of staleness — but the probe's own findings still have to show.
    renderBanner(
      migration({ staleness: { ...STALE, known: false, stale: false } }),
    );
    expect(screen.getByText(/probed directly/i)).toBeInTheDocument();
    expect(screen.getByText(/The probe found these missing/i)).toBeInTheDocument();
    // No version comparison happened, so none is implied.
    expect(screen.queryByText(/Running on a saved image/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/out of date/i)).not.toBeInTheDocument();
  });

  it("stays quiet for an unlabelled container the probe found nothing wrong with", () => {
    const { container } = renderBanner(
      migration({
        staleness: {
          ...FRESH,
          known: false,
          stale: false,
          outdated_package_count: 3,
        },
      }),
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("speaks up when an unlabelled container could not be probed at all", () => {
    // The probe is the only signal a container with no lineage label has. If
    // it fails and the banner stays silent, that is indistinguishable from
    // "up to date" — the exact reading that let an out-of-date project go
    // unnoticed indefinitely.
    renderBanner(
      migration({
        staleness: {
          ...FRESH,
          known: false,
          stale: false,
          probe_error: "output exceeded the inspection limit",
        },
        probeSettled: false,
      }),
    );
    expect(
      screen.getByText(/Container base could not be checked/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/output exceeded the inspection limit/i),
    ).toBeInTheDocument();
    // And it must not pose as a finding about the container itself.
    expect(
      screen.queryByText(/Container is missing things/i),
    ).not.toBeInTheDocument();
  });

  it("stays quiet when a labelled container's probe fails but its lineage is current", () => {
    // `known` means the version comparison already answered the question, so
    // a failed probe is not grounds to raise anything.
    const { container } = renderBanner(
      migration({
        staleness: { ...FRESH, probe_error: "could not exec in the container" },
        probeSettled: false,
      }),
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("disables the action and explains why while the container is running", () => {
    renderBanner(migration({ staleness: STALE }), false);
    expect(
      screen.getByRole("button", { name: /Update container base/i }),
    ).toBeDisabled();
    expect(screen.getByText(/Stop the container to update its base/i)).toBeInTheDocument();
  });

  it("keeps reporting an in-flight run after the modal is closed", () => {
    renderBanner(
      migration({
        staleness: STALE,
        running: true,
        phaseMessage: "Reinstalling socat…",
      }),
    );
    expect(screen.getByText(/Updating container base/i)).toBeInTheDocument();
    expect(screen.getByText("Reinstalling socat…")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Show progress/i })).toBeInTheDocument();
  });

  it("surfaces a run recovered from a crash", () => {
    renderBanner(migration({ staleness: STALE, running: true, recovered: true }));
    expect(
      screen.getByText(/A container base update was already running/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/still in progress when the app last closed/i),
    ).toBeInTheDocument();
  });

  it("does not let an interrupted migration hide behind a plain staleness notice", () => {
    const m = migration({
      staleness: STALE,
      interrupted: {
        phase: "interrupted",
        from_image_id: "sha256:aaa",
        to_base_id: "sha256:bbb",
        started_at: "2026-08-09T10:00:00Z",
        report: null,
        rollback_image: "triple-c-snapshot-p1:pre-migration-1754733600",
        staging_path: null,
        options: { replay_packages: true, copy_paths: false, keep_rollback: true },
        plan: null,
      },
    });
    renderBanner(m);
    expect(
      screen.getByText(/The container base update did not finish/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/part-way onto the new base/i)).toBeInTheDocument();
    // The plain "Update container base…" call to action must not be what is
    // offered here — the container is mid-swap, so it is resume or roll back.
    expect(
      screen.queryByRole("button", { name: /Update container base/i }),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Resume update" }));
    expect(m.resume).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Roll back" })).toBeInTheDocument();
  });

  it("offers no rollback for an interrupted run that kept no rollback image", () => {
    renderBanner(
      migration({
        staleness: STALE,
        interrupted: {
          phase: "interrupted",
          from_image_id: "sha256:aaa",
          to_base_id: "sha256:bbb",
          started_at: "2026-08-09T10:00:00Z",
          report: null,
          rollback_image: null,
          staging_path: null,
          options: { replay_packages: true, copy_paths: false, keep_rollback: false },
          plan: null,
        },
      }),
    );
    expect(screen.queryByRole("button", { name: "Roll back" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Resume update" })).toBeInTheDocument();
  });

  it("distinguishes an unsettled probe from a running container", () => {
    // "Stop the container to update its base" on a container that is already
    // stopped — because the probe has not landed — reads as a bug.
    renderBanner(
      migration({ staleness: STALE, probing: true, probeSettled: false }),
      false,
    );
    expect(
      screen.getByText(/Checking what this container has/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Stop the container to update its base/i),
    ).not.toBeInTheDocument();
  });

  it("names the /var data that updating would destroy", () => {
    renderBanner(
      migration({
        staleness: {
          ...STALE,
          unpreserved_data: [
            { path: "/var/lib/postgresql", bytes: 41_000_000, file_count: 912 },
          ],
        },
      }),
    );
    expect(screen.getByText("/var/lib/postgresql")).toBeInTheDocument();
    expect(screen.getByText(/back this up before updating/i)).toBeInTheDocument();
  });

  describe("the report", () => {
    const CLEAN: MigrationReport = {
      phase: "succeeded",
      packages_requested: ["socat", "bubblewrap"],
      packages_installed: [
        "socat",
        "bubblewrap",
        "ca-certificates",
        "openssl",
        "curl",
        "jq",
        "ripgrep",
        "unzip",
      ],
      packages_failed: [],
      paths_copied: [],
      features_restored: [
        "Host-browser opening",
        "Auth bridge tunnel (socat)",
        "Sandbox mode (bubblewrap)",
        "Mission Control",
      ],
      rollback_available: true,
      message: "",
    };

    const PARTIAL: MigrationReport = {
      phase: "partial",
      packages_requested: ["socat", "bubblewrap", "libfoo-dev"],
      packages_installed: ["socat"],
      packages_failed: [
        { name: "bubblewrap", reason: "held back by apt-mark" },
        { name: "libfoo-dev", reason: "no installation candidate in noble" },
      ],
      paths_copied: [],
      features_restored: ["Auth bridge tunnel (socat)"],
      rollback_available: true,
      message: "",
    };

    it("reports a clean run with counts and both choices", () => {
      renderBanner(migration({ staleness: FRESH, report: CLEAN }));
      expect(screen.getByText(/8 packages reinstalled/i)).toBeInTheDocument();
      expect(screen.getByText(/4 features restored/i)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Keep" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Roll back" })).toBeInTheDocument();
    });

    it("names every failed package and why, and does not read as a success", () => {
      renderBanner(migration({ staleness: STALE, report: PARTIAL }));
      expect(screen.getByText(/Updated, but not completely/i)).toBeInTheDocument();
      expect(screen.getByText("bubblewrap")).toBeInTheDocument();
      expect(screen.getByText(/held back by apt-mark/)).toBeInTheDocument();
      expect(screen.getByText("libfoo-dev")).toBeInTheDocument();
      expect(
        screen.getByText(/no installation candidate in noble/),
      ).toBeInTheDocument();
      // And the exact line that finishes the job by hand.
      expect(
        screen.getByText("sudo apt-get install -y bubblewrap libfoo-dev"),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: /Copy apt-get line/i }),
      ).toBeInTheDocument();
    });

    it("says a failed run has already been restored, and offers no rollback", () => {
      renderBanner(
        migration({
          staleness: STALE,
          report: {
            phase: "failed",
            packages_requested: [],
            packages_installed: [],
            packages_failed: [],
            paths_copied: [],
            features_restored: [],
            rollback_available: false,
            message:
              "Update failed at replay. Your container has been restored to its previous state.",
          },
        }),
      );
      expect(screen.getByText(/Update failed at replay/i)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Roll back" })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Dismiss" })).toBeInTheDocument();
    });

    it("never offers Keep over a container that is still mid-swap", () => {
      // The failing-commit path returns a report *and* leaves the record
      // interrupted. Keep would untag the rollback image and delete the record
      // while `triple-c-snapshot-<id>:latest` still points at the old lineage —
      // and the backend's own message on that record says to resume.
      const m = migration({
        staleness: STALE,
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
          ...CLEAN,
          phase: "failed",
          message: "saving it failed. Resume it, or roll back.",
        },
      });
      renderBanner(m);
      expect(screen.queryByRole("button", { name: "Keep" })).not.toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Resume update" }),
      ).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Roll back" }));
      expect(m.rollback).toHaveBeenCalledTimes(1);
    });

    it("does not describe rollback as a time machine", () => {
      renderBanner(migration({ staleness: FRESH, report: CLEAN }));
      expect(
        screen.getByText(/Rollback restores the system layer only/i),
      ).toBeInTheDocument();
      expect(screen.getByText(/Volumes are never touched/i)).toBeInTheDocument();
    });
  });
});
