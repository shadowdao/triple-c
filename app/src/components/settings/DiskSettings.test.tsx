import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor, within } from "@testing-library/react";
import DiskSettings from "./DiskSettings";
import type {
  DestructiveItem,
  DiskUsageReport,
  ProjectDiskRow,
  ReclaimItem,
  ReclaimPlan,
  ReclaimResult,
  ReclaimTarget,
} from "../../lib/types";

const getDockerDiskUsage = vi.fn();
const listReclaimable = vi.fn();
const reclaim = vi.fn();
const destroyProjectDiskObject = vi.fn();

vi.mock("../../lib/tauri-commands", () => ({
  getDockerDiskUsage: () => getDockerDiskUsage(),
  listReclaimable: (report: DiskUsageReport) => listReclaimable(report),
  reclaim: (targets: ReclaimTarget[]) => reclaim(targets),
  destroyProjectDiskObject: (target: unknown, confirmation: string) =>
    destroyProjectDiskObject(target, confirmation),
  sweepOrphanedSnapshots: vi.fn(async () => ({})),
}));

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const row = (over: Partial<ProjectDiskRow> = {}): ProjectDiskRow => ({
  project_id: "p-whp",
  project_name: "whp",
  snapshot_image: "triple-c-snapshot-p-whp:latest",
  snapshot_exists: true,
  snapshot_bytes: 12_273_392_374,
  snapshot_shared_bytes: 3_832_425_659,
  snapshot_commit_layers: 14,
  base_lineage_known: true,
  snapshot_above_base_bytes: 8_440_966_715,
  container_exists: true,
  container_running: false,
  container_writable_bytes: 868_000_000,
  home_volume_bytes: 4_860_000_000,
  home_volume_present: true,
  config_volume_bytes: 427_000_000,
  config_volume_present: true,
  // The one figure the Snapshot column shows and the Total is built from.
  // 8.44 + 0.868 + 4.86 + 0.427 == 14.596, and the table is expected to add up.
  snapshot_attributed_bytes: 8_440_966_715,
  total_bytes: 14_595_966_715,
  migrating: false,
  ...over,
});

const report = (over: Partial<DiskUsageReport> = {}): DiskUsageReport => ({
  scanned_at: "2026-08-23T10:00:00Z",
  projects: [row()],
  base_images: [
    {
      reference: "ghcr.io/shadowdao/triple-c-sandbox:latest",
      bytes: 4_724_062_366,
      shared_bytes: 4_723_860_396,
      containers: 2,
      is_labelled_base: true,
    },
  ],
  base_images_bytes: 4_724_062_366,
  orphan_image_bytes: 11_900_000_000,
  orphan_image_count: 3,
  orphan_volumes: [],
  orphan_volume_bytes: 0,
  orphan_volumes_unavailable: null,
  build_cache: {
    total_bytes: 28_000_000_000,
    reclaimable_bytes: 28_000_000_000,
    stale_bytes: 20_000_000_000,
    source: "buildx du",
    cli_error: null,
  },
  images_total_bytes: 104_500_000_000,
  containers_total_bytes: 7_497_000_000,
  volumes_total_bytes: 72_890_000_000,
  triple_c_total_bytes: 116_000_000_000,
  host: {
    docker_root_dir: "/var/lib/docker",
    operating_system: "Docker Desktop",
    is_docker_desktop: true,
    is_windows_host: false,
    vhdx_applies: false,
    vhdx_note: "",
    vhdx_fix: [],
    vhdx_fix_gui: "",
  },
  ...over,
});

const item = (over: Partial<ReclaimItem> = {}): ReclaimItem => ({
  target: { kind: "dangling_snapshots" },
  safety: "safe",
  daemon_wide: false,
  label: "Superseded snapshot layers (3 images)",
  detail: "Untagged images left behind by past container recreations.",
  bytes: 11_900_000_000,
  bytes_are_exact: true,
  bytes_floor: null,
  blocked: null,
  ...over,
});

const result = (over: Partial<ReclaimResult> = {}): ReclaimResult => ({
  target: { kind: "dangling_snapshots" },
  destroyed: null,
  ok: true,
  freed_bytes: 0,
  projected_bytes: null,
  message: "Removed 3 images.",
  ...over,
});

/** An orphaned volume, as `list_reclaimable` now describes it: a
 *  `DestructiveItem`, never a `ReclaimItem`. `project_name` carries the
 *  *volume* name, because there is no project to name — that is the definition
 *  of the variant, and it is what `destroy` compares the typed string against. */
const orphan = (over: Partial<DestructiveItem> = {}): DestructiveItem => ({
  target: {
    kind: "orphan_volume",
    name: "triple-c-claude-config-gone",
    project_id: "gone",
  },
  project_id: "gone",
  project_name: "triple-c-claude-config-gone",
  label: "triple-c-claude-config-gone (config volume)",
  loses:
    "Named for project id gone, which is not in Triple-C's project list, and no container is attached to it. Docker created it on 2026-03-14T09:00:00Z. This is a `.claude` volume — it held that project's Claude credential, plugins and session transcripts. Not recoverable. Type the volume name to confirm.",
  bytes: 900_000,
  blocked: null,
  ...over,
});

const plan = (over: Partial<ReclaimPlan> = {}): ReclaimPlan => ({
  items: [item()],
  destructive: [],
  store_error: null,
  ...over,
});

async function renderAndScan() {
  render(<DiskSettings />);
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Scan" }));
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  getDockerDiskUsage.mockResolvedValue(report());
  listReclaimable.mockResolvedValue(plan());
  reclaim.mockResolvedValue({ results: [], total_freed_bytes: 0 });
});

// ---------------------------------------------------------------------------

describe("DiskSettings", () => {
  it("never scans until the button is pressed", async () => {
    // `df()` walks the whole daemon and takes seconds on a large store, and
    // AccordionSection unmounts its body when collapsed — so a scan on mount
    // would re-run every time the section was opened.
    render(<DiskSettings />);
    await act(async () => {
      await Promise.resolve();
    });
    expect(getDockerDiskUsage).not.toHaveBeenCalled();
    expect(screen.getByText(/never done for you/)).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Scan" }));
    });
    expect(getDockerDiskUsage).toHaveBeenCalledTimes(1);
  });

  it("says it is scanning in words, not only in colour", async () => {
    let resolve: (value: DiskUsageReport) => void = () => {};
    getDockerDiskUsage.mockReturnValue(
      new Promise<DiskUsageReport>((r) => {
        resolve = r;
      }),
    );
    render(<DiskSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Scan" }));
    expect(screen.getByText("Scanning")).toBeInTheDocument();
    await act(async () => {
      resolve(report());
    });
    await waitFor(() => expect(screen.getByText(/^Scanned /)).toBeInTheDocument());
  });

  it("shows the layer count and the cost of the next commit", async () => {
    // The two numbers that explain the growth mechanism. A total alone never
    // says why the disk filled up.
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(within(projectRow).getByText("14")).toBeInTheDocument();
    expect(within(projectRow).getByText("+868.0 MB")).toBeInTheDocument();
    expect(within(projectRow).getByText("14.6 GB")).toBeInTheDocument();
  });

  it("refuses to present a layer count that does not mean recreations", async () => {
    // Without `triple-c.base-image-id` — the normal case for a project created
    // before that label existed — the count includes the base's own ~15 layers.
    // Printing it beside a header that says "one per recreation" would be a
    // wrong number in the column the table exists for.
    getDockerDiskUsage.mockResolvedValue(
      report({ projects: [row({ base_lineage_known: false, snapshot_commit_layers: 17 })] }),
    );
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(within(projectRow).getByText("unknown")).toBeInTheDocument();
    expect(within(projectRow).queryByText("17")).not.toBeInTheDocument();
  });

  it("builds the Snapshot column and the Total from the same attributed figure", async () => {
    // The bug this pins: the column rendered `snapshot_above_base_bytes` and
    // fell back to `—`, while the total was `snapshot_bytes -
    // snapshot_shared_bytes` regardless — which in the fallback branch is the
    // whole 4.7 GB base image, charged to every row and then added again as a
    // base-image row in the globals. One field, computed once in Rust.
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(within(projectRow).getByText("8.4 GB")).toBeInTheDocument();
    expect(within(projectRow).getByText("14.6 GB")).toBeInTheDocument();
  });

  it("says a snapshot figure is the whole image rather than passing it off as a share", async () => {
    // `snapshot_above_base_bytes` is null in exactly one branch: nothing
    // measurably shares layers with the snapshot *and* its base is gone. The
    // attributed figure is then the whole image — an honest cost, not a guess
    // and not zero — but it does not mean what the other rows' figures mean,
    // so the sub-line has to say which one this is.
    getDockerDiskUsage.mockResolvedValue(
      report({
        projects: [
          row({
            snapshot_shared_bytes: 0,
            snapshot_above_base_bytes: null,
            snapshot_attributed_bytes: 12_273_392_374,
            total_bytes: 18_428_392_374,
          }),
        ],
      }),
    );
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(within(projectRow).queryByText("0 B")).not.toBeInTheDocument();
    expect(within(projectRow).getByText("12.3 GB")).toBeInTheDocument();
    expect(projectRow.textContent).toMatch(/whole image — base unknown/);
    // And it must not still claim the "N with base" split it cannot measure.
    expect(projectRow.textContent).not.toMatch(/with base/);
  });

  it("marks a heavily stacked snapshot with a word, not just a colour", async () => {
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(within(projectRow).getByText("stacked")).toBeInTheDocument();
  });

  it("charges the shared base to the globals, not to every project row", async () => {
    // The base is one 4.7 GB image every project descends from. Counting it per
    // row would show it eight times and make the column meaningless.
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(within(projectRow).getByText("8.4 GB")).toBeInTheDocument();
    expect(within(projectRow).getByText(/12\.3 GB with base/)).toBeInTheDocument();
  });

  it("plans from the report it already has rather than scanning twice", async () => {
    await renderAndScan();
    await waitFor(() => expect(listReclaimable).toHaveBeenCalledTimes(1));
    expect(getDockerDiskUsage).toHaveBeenCalledTimes(1);
    expect(listReclaimable).toHaveBeenCalledWith(expect.objectContaining({ projects: expect.any(Array) }));
  });

  // -------------------------------------------------------------------------
  // Selection plumbing
  // -------------------------------------------------------------------------

  it("sends exactly the ticked targets and nothing else", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        items: [
          item(),
          item({
            target: { kind: "migration_staging" },
            label: "Migration staging files",
            bytes: 500_000_000,
          }),
        ],
      }),
    );
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");

    const boxes = screen.getAllByRole("checkbox");
    await act(async () => {
      fireEvent.click(boxes[1]);
    });
    expect(screen.getByText(/1 selected, 500\.0 MB/)).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reclaim" }));
    });
    expect(reclaim).toHaveBeenCalledWith([{ kind: "migration_staging" }]);
  });

  it("clears the tick list once the reclaim has run", async () => {
    // The plan's rows describe objects the reclaim just removed; leaving them
    // ticked lets the user fire the same call again against nothing.
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    fireEvent.click(screen.getAllByRole("checkbox")[0]);
    expect(screen.getByText(/1 selected/)).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reclaim" }));
    });
    expect(screen.queryByTestId("disk-safe-bucket")).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    // And it says why the list is gone rather than claiming nothing was found.
    expect(screen.getByTestId("disk-plan-stale").textContent).toMatch(
      /measured before that last action/,
    );
  });

  it("says why the build-cache figure is the under-reporting one", async () => {
    // Without this, a `buildx du` failure silently shows `docker system df`'s
    // number, which under-reports what a prune would free.
    getDockerDiskUsage.mockResolvedValue(
      report({
        build_cache: {
          total_bytes: 28_000_000_000,
          reclaimable_bytes: 1_000_000,
          stale_bytes: 0,
          source: "system df",
          cli_error: "`docker buildx du` failed: executable not found",
        },
      }),
    );
    await renderAndScan();
    const globals = await screen.findByTestId("disk-globals");
    expect(globals.textContent).toMatch(/under-reports what a prune would free/);
    expect(globals.textContent).toMatch(/executable not found/);
  });

  it("cannot reclaim with nothing ticked", async () => {
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    expect(screen.getByRole("button", { name: "Reclaim" })).toBeDisabled();
    expect(screen.getByText("Nothing ticked.")).toBeInTheDocument();
  });

  it("refuses to tick a blocked item", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        items: [item({ blocked: "A base-image migration is in flight for this project." })],
      }),
    );
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    const box = screen.getByRole("checkbox");
    expect(box).toBeDisabled();
    expect(
      screen.getByText("A base-image migration is in flight for this project."),
    ).toBeInTheDocument();
  });

  it("keeps semi-safe work out of the one-button bucket", async () => {
    // Compaction is a rewrite and cache clearing costs a re-download. Neither
    // may be swept up by a Reclaim press aimed at the free wins.
    listReclaimable.mockResolvedValue(
      plan({
        items: [
          item(),
          item({
            target: { kind: "compact_snapshot", project_id: "p-whp" },
            safety: "semi_safe",
            label: "Compact whp's snapshot",
            bytes: 5_100_000_000,
            bytes_are_exact: false,
            bytes_floor: 0,
          }),
        ],
      }),
    );
    await renderAndScan();

    const safe = await screen.findByTestId("disk-safe-bucket");
    expect(within(safe).getAllByRole("checkbox")).toHaveLength(1);
    expect(within(safe).queryByText(/Compact whp/)).not.toBeInTheDocument();

    const semi = screen.getByTestId("disk-semi-bucket");
    expect(within(semi).getByText("Compact whp's snapshot")).toBeInTheDocument();
    expect(within(semi).queryByRole("checkbox")).not.toBeInTheDocument();
  });

  it("marks a compaction's yield as a bound, never as a measurement", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        items: [
          item({
            target: { kind: "compact_snapshot", project_id: "p-whp" },
            safety: "semi_safe",
            label: "Compact whp's snapshot",
            bytes: 5_100_000_000,
            bytes_are_exact: false,
            bytes_floor: 0,
          }),
        ],
      }),
    );
    await renderAndScan();
    const semi = await screen.findByTestId("disk-semi-bucket");
    expect(within(semi).getByText("up to 5.1 GB")).toBeInTheDocument();
  });

  it("says out loud when an action reaches the whole daemon", async () => {
    // The user's daemon also holds unrelated postgres and site-builder work,
    // and a build-cache prune takes their warm cache with ours.
    listReclaimable.mockResolvedValue(
      plan({
        items: [
          item({
            target: { kind: "build_cache", all: true },
            daemon_wide: true,
            label: "Build cache, all of it",
            bytes: 28_000_000_000,
          }),
        ],
      }),
    );
    await renderAndScan();
    const safe = await screen.findByTestId("disk-safe-bucket");
    expect(within(safe).getByText("whole daemon")).toBeInTheDocument();
  });

  // -------------------------------------------------------------------------
  // Orphan copy — the correction that matters most
  // -------------------------------------------------------------------------

  it("says what a 'no matching project' volume is derived from", async () => {
    // An idle live project has volumes, no container and possibly no image —
    // indistinguishable from a deleted one unless you consult the project
    // store. The copy must not invite the inference that made that mistake.
    getDockerDiskUsage.mockResolvedValue(
      report({
        orphan_volumes: [
          {
            name: "triple-c-home-gone",
            project_id: "gone",
            bytes: 900_000,
            role: "home",
            created_at: "2026-03-14T09:00:00Z",
          },
        ],
        orphan_volume_bytes: 900_000,
      }),
    );
    await renderAndScan();
    const globals = await screen.findByTestId("disk-globals");
    expect(
      within(globals).getByText(/Volumes with no matching project in Triple-C/),
    ).toBeInTheDocument();
    // The sentence is split by an <em>, so match on the container's text.
    expect(globals.textContent).toMatch(/is not inferred from a project being stopped/i);
    expect(globals.textContent).toMatch(
      /A project you have not opened in a while has no container and no snapshot either, and that is normal/i,
    );
  });

  it("never offers an orphaned volume as a tick in the safe bucket", async () => {
    // It used to be a `ReclaimTarget` at `Safety::Safe` — a tick and the group
    // Reclaim button, no confirmation — for a volume holding a Claude
    // credential and every transcript a project ever had. The Rust variant is
    // gone; this pins that the frontend cannot resurrect it.
    listReclaimable.mockResolvedValue(plan({ destructive: [orphan()] }));
    await renderAndScan();
    const safe = await screen.findByTestId("disk-safe-bucket");
    expect(within(safe).getAllByRole("checkbox")).toHaveLength(1);
    expect(safe.textContent).not.toMatch(/triple-c-claude-config-gone/);
    // And it is reachable — an item that matches no project row would
    // otherwise simply vanish from the UI.
    expect(await screen.findByTestId("disk-orphan-bucket")).toBeInTheDocument();
  });

  it("shows a rollback pin whose project is gone, instead of dropping it", async () => {
    // `survey_rollback_pins` walks images, not projects, and falls back to the
    // raw id when the project is absent. The per-project table joins
    // destructive items to rows by `project_id`, and rows come only from
    // projects in the store — so before the unmatched bucket, such a pin was
    // measured by the scan and rendered nowhere at all. A multi-GB image the
    // panel knew about and offered no way to remove.
    const ownerlessPin: DestructiveItem = {
      target: {
        kind: "rollback_pin",
        project_id: "dead0000-0000-0000-0000-000000000000",
        tag: "pre-migration-20260101-101500",
      },
      project_id: "dead0000-0000-0000-0000-000000000000",
      // The Rust falls back to the id, and it is what `destroy` compares
      // against — so this is the string the user has to type.
      project_name: "dead0000-0000-0000-0000-000000000000",
      label: "Rollback pin pre-migration-20260101-101500",
      loses: "The only copy of that migration's rollback target.",
      bytes: 5_400_000_000,
      blocked: null,
    };
    listReclaimable.mockResolvedValue(plan({ destructive: [ownerlessPin] }));
    await renderAndScan();

    const bucket = await screen.findByTestId("disk-unmatched-bucket");
    expect(within(bucket).getByText(/Rollback pin pre-migration-20260101-101500/)).toBeInTheDocument();
    // And it is not silently folded into the project table.
    const table = screen.queryByTestId("disk-project-table");
    if (table) {
      expect(table.textContent).not.toMatch(/pre-migration-20260101-101500/);
    }
  });

  it("asks for the project id, not a project name, when there is no project", async () => {
    // The gate compares against `project_name`, which is the raw id here. That
    // works — but a dialog captioned "type the project name" for a project that
    // no longer exists asks for something the user cannot supply.
    const ownerlessPin: DestructiveItem = {
      target: {
        kind: "rollback_pin",
        project_id: "dead0000-0000-0000-0000-000000000000",
        tag: "pre-migration-20260101-101500",
      },
      project_id: "dead0000-0000-0000-0000-000000000000",
      project_name: "dead0000-0000-0000-0000-000000000000",
      label: "Rollback pin pre-migration-20260101-101500",
      loses: "The only copy of that migration's rollback target.",
      bytes: 5_400_000_000,
      blocked: null,
    };
    listReclaimable.mockResolvedValue(plan({ destructive: [ownerlessPin] }));
    await renderAndScan();

    const bucket = await screen.findByTestId("disk-unmatched-bucket");
    fireEvent.click(within(bucket).getByRole("button", { name: /Delete/ }));

    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toMatch(/project id/i);
    expect(dialog.textContent).not.toMatch(/type the project name/i);
  });

  it("keeps orphaned volumes out of the per-project table", async () => {
    // The table keys off `project_id`, and an orphan's id matches no row by
    // definition. Passing them in anyway is how one would leak into the wrong
    // project's overflow menu if a row ever shared the id.
    listReclaimable.mockResolvedValue(plan({ destructive: [orphan()] }));
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(projectRow.textContent).not.toMatch(/triple-c-claude-config-gone/);
  });

  it("says what a config volume actually holds, not 'volume data'", async () => {
    listReclaimable.mockResolvedValue(plan({ destructive: [orphan()] }));
    await renderAndScan();
    const bucket = await screen.findByTestId("disk-orphan-bucket");
    expect(bucket.textContent).toMatch(/Claude login credential/i);
    expect(bucket.textContent).toMatch(/every plugin and skill installed into it/i);
    expect(bucket.textContent).toMatch(/every conversation transcript it ever had/i);
    // The derivation caveat travels with the offer, not only with the totals.
    expect(bucket.textContent).toMatch(/not.*inferred from a project being stopped/i);
  });

  it("confirms an orphaned volume against its own name, never a project's", async () => {
    listReclaimable.mockResolvedValue(plan({ destructive: [orphan()] }));
    destroyProjectDiskObject.mockResolvedValue({ results: [], total_freed_bytes: 0 });
    await renderAndScan();
    const bucket = await screen.findByTestId("disk-orphan-bucket");
    await act(async () => {
      fireEvent.click(within(bucket).getByRole("button", { name: /Delete/ }));
    });

    const dialog = screen.getByRole("dialog");
    // Asking for "the exact project name" would be asking for a string that
    // does not exist.
    expect(within(dialog).getByRole("status")).toHaveTextContent(
      "Waiting for the exact volume name.",
    );
    const input = within(dialog).getByLabelText(/Type/);
    const confirm = within(dialog).getByRole("button", { name: "Delete volume" });

    // The project id parsed out of the name is display only and must not open
    // the gate.
    fireEvent.change(input, { target: { value: "gone" } });
    expect(confirm).toBeDisabled();

    fireEvent.change(input, { target: { value: "triple-c-claude-config-gone" } });
    expect(confirm).toBeEnabled();
    await act(async () => {
      fireEvent.click(confirm);
    });

    expect(destroyProjectDiskObject).toHaveBeenCalledWith(
      { kind: "orphan_volume", name: "triple-c-claude-config-gone", project_id: "gone" },
      "triple-c-claude-config-gone",
    );
    // One volume, one confirmation — `reclaim` never sees it.
    expect(reclaim).not.toHaveBeenCalled();
  });

  it("explains a suppressed orphan list instead of showing an empty one", async () => {
    // With the project store unreadable every project's volumes look
    // unclaimed. Showing nothing is right; showing nothing *silently* is not.
    getDockerDiskUsage.mockResolvedValue(
      report({
        orphan_volumes: [],
        orphan_volumes_unavailable:
          "projects.json could not be read, so there is no way to tell an orphaned volume from a live project's.",
      }),
    );
    await renderAndScan();
    const banner = await screen.findByTestId("disk-store-error");
    expect(within(banner).getByText("Could not read the project list")).toBeInTheDocument();
    expect(within(banner).getByText(/no way to tell/)).toBeInTheDocument();
  });

  // -------------------------------------------------------------------------
  // Windows / WSL2
  // -------------------------------------------------------------------------

  it("spells out that pruning will not shrink C: on Docker Desktop for Windows", async () => {
    getDockerDiskUsage.mockResolvedValue(
      report({
        host: {
          docker_root_dir: "/var/lib/docker",
          operating_system: "Docker Desktop",
          is_docker_desktop: true,
          is_windows_host: true,
          vhdx_applies: true,
          vhdx_note: "Docker Desktop keeps this daemon inside ext4.vhdx on C:.",
          vhdx_fix: ["wsl --shutdown", 'Optimize-VHD -Path "…docker_data.vhdx" -Mode Full'],
          vhdx_fix_gui: "Docker Desktop → Settings → Resources → Advanced → Clean up / Purge data",
        },
      }),
    );
    await renderAndScan();
    const note = await screen.findByTestId("disk-vhdx-note");
    expect(note.textContent).toMatch(/Warning: reclaiming here will not shrink your C: drive/);
    expect(within(note).getByText(/wsl --shutdown/)).toBeInTheDocument();
    expect(within(note).getByText(/Optimize-VHD/)).toBeInTheDocument();
    expect(within(note).getByText(/Purge data/)).toBeInTheDocument();
  });

  it("keeps the vhdx note off a host it does not apply to", async () => {
    await renderAndScan();
    await screen.findByTestId("disk-globals");
    expect(screen.queryByTestId("disk-vhdx-note")).not.toBeInTheDocument();
  });

  // -------------------------------------------------------------------------
  // Destructive path
  // -------------------------------------------------------------------------

  it("needs the project name typed before it will delete a config volume", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        destructive: [
          {
            target: { kind: "config_volume", project_id: "p-whp" },
            project_id: "p-whp",
            project_name: "whp",
            label: "Claude config volume",
            loses: "The Claude login credential, plugins, and EVERY conversation transcript.",
            bytes: 427_000_000,
            blocked: null,
          },
        ],
      }),
    );
    destroyProjectDiskObject.mockResolvedValue({
      target: null,
      destroyed: { kind: "config_volume", project_id: "p-whp" },
      ok: true,
      freed_bytes: 427_000_000,
      projected_bytes: null,
      message: "Removed volume.",
    });

    await renderAndScan();
    await screen.findByTestId("disk-row-p-whp");

    fireEvent.click(screen.getByRole("button", { name: "Delete whp data" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: /Delete claude config volume/ }));
    });

    const dialog = screen.getByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Delete claude config volume" });
    expect(confirm).toBeDisabled();
    expect(within(dialog).getByText(/EVERY conversation transcript/)).toBeInTheDocument();

    // The wrong name does not open the gate.
    fireEvent.change(within(dialog).getByLabelText(/Type/), { target: { value: "who" } });
    expect(confirm).toBeDisabled();

    fireEvent.change(within(dialog).getByLabelText(/Type/), { target: { value: "whp" } });
    expect(confirm).toBeEnabled();
    await act(async () => {
      fireEvent.click(confirm);
    });
    expect(destroyProjectDiskObject).toHaveBeenCalledWith(
      { kind: "config_volume", project_id: "p-whp" },
      "whp",
    );
  });

  it("keeps the confirmation open and busy while the deletion runs", async () => {
    // The modal used to be unmounted before the call was awaited, which made
    // its whole busy path dead code and left a multi-second volume removal with
    // no indication it was happening.
    listReclaimable.mockResolvedValue(
      plan({
        destructive: [
          {
            target: { kind: "home_volume", project_id: "p-whp" },
            project_id: "p-whp",
            project_name: "whp",
            label: "Home volume",
            loses: "Shell history and toolchains.",
            bytes: 4_860_000_000,
            blocked: null,
          },
        ],
      }),
    );
    let finish: (value: unknown) => void = () => {};
    destroyProjectDiskObject.mockReturnValue(new Promise((r) => (finish = r)));

    await renderAndScan();
    await screen.findByTestId("disk-row-p-whp");
    fireEvent.click(screen.getByRole("button", { name: "Delete whp data" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: /Delete home volume/ }));
    });

    const dialog = screen.getByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Type/), { target: { value: "whp" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete home volume" }));

    // Still open, and saying so.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Working…" })).toBeDisabled(),
    );

    await act(async () => {
      finish({
        target: null,
        destroyed: { kind: "home_volume", project_id: "p-whp" },
        ok: true,
        freed_bytes: 4_860_000_000,
        projected_bytes: null,
        message: "Removed volume.",
      });
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("never routes a destructive object through the bulk Reclaim button", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        destructive: [
          {
            target: { kind: "home_volume", project_id: "p-whp" },
            project_id: "p-whp",
            project_name: "whp",
            label: "Home volume",
            loses: "Shell history, dotfiles, toolchains.",
            bytes: 4_860_000_000,
            blocked: null,
          },
        ],
      }),
    );
    await renderAndScan();
    const safe = await screen.findByTestId("disk-safe-bucket");
    // One tick, for the dangling images — the home volume is not in this list
    // at any price.
    expect(within(safe).getAllByRole("checkbox")).toHaveLength(1);
    expect(within(safe).queryByText(/Home volume/)).not.toBeInTheDocument();
  });

  it("reports what was actually freed against what was projected", async () => {
    reclaim.mockResolvedValue({
      results: [
        {
          target: { kind: "compact_snapshot", project_id: "p-whp" },
          destroyed: null,
          ok: true,
          freed_bytes: 5_100_000_000,
          projected_bytes: 7_000_000_000,
          message: "Rewrote the snapshot into a single layer.",
        },
      ],
      total_freed_bytes: 5_100_000_000,
    });
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    fireEvent.click(screen.getAllByRole("checkbox")[0]);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reclaim" }));
    });

    const outcome = await screen.findByTestId("disk-outcome");
    expect(within(outcome).getByText("Reclaimed 5.1 GB")).toBeInTheDocument();
    expect(within(outcome).getByText(/projected up to 7\.0 GB, actually 5\.1 GB/)).toBeInTheDocument();
  });

  // -------------------------------------------------------------------------
  // Failure has to reach the words, and the place the user is looking
  // -------------------------------------------------------------------------

  it("puts a partial failure in the headline, not only in the glyph's hue", async () => {
    // This panel is where the "never encode status in colour alone" rule is
    // documented, and the outcome headline used to say "Reclaimed 1.2 GB" for
    // a run where most of the targets threw — only the glyph and its colour
    // changed, which is exactly nothing to a screen reader or to anyone who
    // does not read red as bad.
    reclaim.mockResolvedValue({
      results: [
        result({ freed_bytes: 1_200_000_000 }),
        result({ target: { kind: "migration_pins" } }),
        result({ target: { kind: "probe_containers" } }),
        result({ target: { kind: "build_cache", all: true }, ok: false }),
        result({ target: { kind: "scrub_containers" }, ok: false }),
      ],
      total_freed_bytes: 1_200_000_000,
    });
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    fireEvent.click(screen.getAllByRole("checkbox")[0]);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reclaim" }));
    });

    const outcome = await screen.findByTestId("disk-outcome");
    expect(
      within(outcome).getByText("Reclaimed 1.2 GB — 2 of 5 failed"),
    ).toBeInTheDocument();
  });

  it("keeps the plain wording when every target succeeded", async () => {
    reclaim.mockResolvedValue({
      results: [result({ freed_bytes: 1_200_000_000 }), result()],
      total_freed_bytes: 1_200_000_000,
    });
    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    fireEvent.click(screen.getAllByRole("checkbox")[0]);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reclaim" }));
    });

    const outcome = await screen.findByTestId("disk-outcome");
    expect(within(outcome).getByText("Reclaimed 1.2 GB")).toBeInTheDocument();
    expect(outcome.textContent).not.toMatch(/failed/);
  });

  it("keeps the typed confirmation open, and says why, when the deletion fails", async () => {
    // The dialog used to close regardless, leaving the failure in a line at
    // the very top of a panel the user had scrolled past to reach the row.
    listReclaimable.mockResolvedValue(
      plan({
        destructive: [
          {
            target: { kind: "home_volume", project_id: "p-whp" },
            project_id: "p-whp",
            project_name: "whp",
            label: "Home volume",
            loses: "Shell history and toolchains.",
            bytes: 4_860_000_000,
            blocked: null,
          },
        ],
      }),
    );
    destroyProjectDiskObject.mockRejectedValue(
      "volume triple-c-home-p-whp is in use by a running container",
    );

    await renderAndScan();
    await screen.findByTestId("disk-row-p-whp");
    fireEvent.click(screen.getByRole("button", { name: "Delete whp data" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: /Delete home volume/ }));
    });

    const dialog = screen.getByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Type/), { target: { value: "whp" } });
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Delete home volume" }));
    });

    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(within(screen.getByRole("dialog")).getByRole("alert")).toHaveTextContent(
      /in use by a running container/,
    );
  });

  it("keeps the semi-safe confirmation open when the action fails", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        items: [
          item({
            target: { kind: "compact_snapshot", project_id: "p-whp" },
            safety: "semi_safe",
            label: "Compact whp's snapshot",
            bytes: 5_100_000_000,
            bytes_are_exact: false,
            bytes_floor: 0,
          }),
        ],
      }),
    );
    reclaim.mockRejectedValue("compaction failed: no space left on device");

    await renderAndScan();
    const semi = await screen.findByTestId("disk-semi-bucket");
    await act(async () => {
      fireEvent.click(within(semi).getByRole("button", { name: "Run…" }));
    });
    await act(async () => {
      fireEvent.click(
        within(screen.getByRole("dialog")).getByRole("button", { name: "Run it" }),
      );
    });

    const dialog = screen.getByRole("dialog");
    expect(dialog).toBeInTheDocument();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(/no space left on device/);
  });

  it("closes the confirmation once the action succeeds", async () => {
    listReclaimable.mockResolvedValue(
      plan({
        items: [
          item({
            target: { kind: "clear_caches", project_id: "p-whp", include_rustup: false },
            safety: "semi_safe",
            label: "Clear whp's caches",
          }),
        ],
      }),
    );
    await renderAndScan();
    const semi = await screen.findByTestId("disk-semi-bucket");
    await act(async () => {
      fireEvent.click(within(semi).getByRole("button", { name: "Run…" }));
    });
    await act(async () => {
      fireEvent.click(
        within(screen.getByRole("dialog")).getByRole("button", { name: "Run it" }),
      );
    });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  // -------------------------------------------------------------------------
  // Scan status: announced, and not startable mid-mutation
  // -------------------------------------------------------------------------

  it("announces the scan status through a live region", async () => {
    // The status flips between three states with no other signal; without a
    // live region wrapping it the change is silent.
    render(<DiskSettings />);
    const live = screen.getByRole("status");
    expect(live).toHaveAttribute("aria-live", "polite");
    expect(live).toHaveTextContent("Not scanned");

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Scan" }));
    });
    // The glyph is `aria-hidden` but still part of `textContent`.
    expect(screen.getByRole("status")).toHaveTextContent(/Scanned \d/);
  });

  it("cannot start a scan while a reclaim is still running", async () => {
    // A scan launched on top of a mutation measures a daemon that is being
    // changed underneath it — the hook can only throw such a result away, so
    // the seconds are better not spent.
    let finish: (value: unknown) => void = () => {};
    reclaim.mockReturnValue(new Promise((r) => (finish = r)));

    await renderAndScan();
    await screen.findByTestId("disk-safe-bucket");
    fireEvent.click(screen.getAllByRole("checkbox")[0]);
    fireEvent.click(screen.getByRole("button", { name: "Reclaim" }));

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Scan again" })).toBeDisabled(),
    );

    await act(async () => {
      finish({ results: [], total_freed_bytes: 0 });
    });
    expect(screen.getByRole("button", { name: "Scan again" })).toBeEnabled();
  });

  it("gives the unknown layer count its explanation without a hover", async () => {
    // The tooltip portals a div with no role and no `aria-describedby`, and
    // wrapped around children it has no focus handlers either — so without the
    // sr-only copy "unknown" reads as a bug to everyone not using a mouse.
    getDockerDiskUsage.mockResolvedValue(
      report({ projects: [row({ base_lineage_known: false, snapshot_commit_layers: 17 })] }),
    );
    await renderAndScan();
    const projectRow = await screen.findByTestId("disk-row-p-whp");
    expect(projectRow.textContent).toMatch(/predates the base-image label/);
    expect(projectRow.textContent).toMatch(/Migrating it to the current base restores the count/);
  });

  it("surfaces a scan failure as an alert", async () => {
    getDockerDiskUsage.mockRejectedValue("Could not read Docker disk usage: no such host");
    render(<DiskSettings />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Scan" }));
    });
    expect(screen.getByRole("alert")).toHaveTextContent(/no such host/);
  });
});
