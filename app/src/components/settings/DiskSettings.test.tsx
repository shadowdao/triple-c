import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor, within } from "@testing-library/react";
import DiskSettings from "./DiskSettings";
import type {
  DiskUsageReport,
  ProjectDiskRow,
  ReclaimItem,
  ReclaimPlan,
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
  snapshot_above_base_bytes: 8_440_966_715,
  container_exists: true,
  container_running: false,
  container_writable_bytes: 868_000_000,
  home_volume_bytes: 4_860_000_000,
  home_volume_present: true,
  config_volume_bytes: 427_000_000,
  config_volume_present: true,
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
    expect(
      within(note).getByText("Reclaiming here will not shrink your C: drive"),
    ).toBeInTheDocument();
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
      target: { kind: "orphan_volume", name: "triple-c-claude-config-p-whp" },
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

  it("surfaces a scan failure rather than showing stale numbers", async () => {
    getDockerDiskUsage.mockRejectedValue("Could not read Docker disk usage: no such host");
    render(<DiskSettings />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Scan" }));
    });
    expect(screen.getByRole("alert")).toHaveTextContent(/no such host/);
  });
});
