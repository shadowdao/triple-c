import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings, Project } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

const applyMarketplaceNow = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  applyMarketplaceNow: (id?: string) => applyMarketplaceNow(id),
}));
vi.mock("./UpdateDiffModal", () => ({
  default: ({ onAccept }: { onAccept: () => Promise<boolean> }) => (
    <button onClick={() => void onAccept()}>accept diff</button>
  ),
}));

import InstalledPane from "./InstalledPane";

const A = "a".repeat(40);
const B = "b".repeat(40);

function api(patch: Partial<MarketplaceApi> = {}): MarketplaceApi {
  return {
    snapshots: [],
    updates: [],
    loading: false,
    refreshing: [],
    load: vi.fn(),
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(),
    uninstall: vi.fn(async () => true),
    setDisabled: vi.fn(),
    update: vi.fn(async () => true),
    forget: vi.fn(async () => true),
    remove: vi.fn(),
    ...patch,
  };
}

describe("InstalledPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      toasts: [],
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://x/y.git", branch: null, account_id: null }],
        marketplace_accounts: [],
        global_marketplace_installs: [
          { marketplace_id: "m1", kind: "agent", key: "rev", commit: A },
          { marketplace_id: "gone", kind: "skill", key: "old", commit: A },
        ],
      } as unknown as AppSettings,
      projects: [
        {
          id: "p1",
          name: "api",
          status: "running",
          marketplace_installs: [{ marketplace_id: "m1", kind: "command", key: "cmd", commit: B }],
          marketplace_disabled: [],
        },
      ] as unknown as Project[],
    });
  });

  it("lists global and project installs", () => {
    render(<InstalledPane mp={api()} />);
    const global = screen.getByTestId("installed-global");
    expect(within(global).getByText("rev")).toBeInTheDocument();
    const proj = screen.getByTestId("installed-project-p1");
    expect(within(proj).getByText("cmd")).toBeInTheDocument();
  });

  it("badges and accepts an update for the matching install", async () => {
    const mp = api({
      updates: [{ item: { marketplace_id: "m1", kind: "agent", key: "rev" }, pinned: A, head: B, invalid_at_head: null }],
    });
    render(<InstalledPane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Review update for rev" }));
    fireEvent.click(screen.getByRole("button", { name: "accept diff" }));
    await waitFor(() =>
      expect(mp.update).toHaveBeenCalledWith({ marketplace_id: "m1", kind: "agent", key: "rev" }, { type: "global" }, B),
    );
  });

  it("PR review #8: an update belongs only to the install pinned at its commit", () => {
    const C = "c".repeat(40);
    // The same agent is installed globally at A and in p1 at C. Only the A
    // install has an update (A → B); C is unchanged at head.
    useAppState.setState({
      projects: [
        {
          id: "p1",
          name: "api",
          status: "running",
          marketplace_installs: [{ marketplace_id: "m1", kind: "agent", key: "rev", commit: C }],
          marketplace_disabled: [],
        },
      ] as unknown as Project[],
    });
    const mp = api({
      updates: [{ item: { marketplace_id: "m1", kind: "agent", key: "rev" }, pinned: A, head: B, invalid_at_head: null }],
    });
    render(<InstalledPane mp={mp} />);
    const global = screen.getByTestId("installed-global");
    expect(within(global).getByRole("button", { name: "Review update for rev" })).toBeInTheDocument();
    const proj = screen.getByTestId("installed-project-p1");
    expect(within(proj).queryByRole("button", { name: "Review update for rev" })).not.toBeInTheDocument();
  });

  it("re-review round 2: an update that would be refused shows why instead of a Review button", () => {
    const mp = api({
      updates: [
        {
          item: { marketplace_id: "m1", kind: "agent", key: "rev" },
          pinned: A,
          head: B,
          invalid_at_head: 'unknown hook event "PreFoo"',
        },
      ],
    });
    render(<InstalledPane mp={mp} />);
    const global = screen.getByTestId("installed-global");
    expect(within(global).queryByRole("button", { name: "Review update for rev" })).not.toBeInTheDocument();
    expect(within(global).getByText(/Update to bbbbbbbb cannot be installed: unknown hook event "PreFoo"/)).toBeInTheDocument();
  });

  it("I2: accepts the head that was reviewed even if the update list moves on", async () => {
    const C = "c".repeat(40);
    const item = { marketplace_id: "m1", kind: "agent" as const, key: "rev" };
    const mp = api({ updates: [{ item, pinned: A, head: B, invalid_at_head: null }] });
    const { rerender } = render(<InstalledPane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Review update for rev" }));
    rerender(<InstalledPane mp={{ ...mp, updates: [{ item, pinned: A, head: C, invalid_at_head: null }] }} />);
    fireEvent.click(screen.getByRole("button", { name: "accept diff" }));
    await waitFor(() => expect(mp.update).toHaveBeenCalledWith(item, { type: "global" }, B));
  });

  it("marks installs whose marketplace was removed and forgets them", () => {
    const mp = api();
    render(<InstalledPane mp={mp} />);
    expect(screen.getByText("Source removed")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Forget installs from removed marketplaces" }));
    expect(mp.forget).toHaveBeenCalledWith("gone");
  });

  it("removes a project install", () => {
    const mp = api();
    render(<InstalledPane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Remove cmd from api" }));
    // F7: the ref passed to uninstall must be the bare item ref, not the
    // MarketplaceInstall (which also carries `commit`).
    expect(mp.uninstall).toHaveBeenCalledWith(
      { marketplace_id: "m1", kind: "command", key: "cmd" },
      { type: "project", project_id: "p1" },
    );
  });

  it("applies now and summarises the result", async () => {
    applyMarketplaceNow.mockResolvedValue([
      { project_id: "p1", report: { installed: ["agent:rev"], updated: [], removed: [], skipped: [], errors: [], finished_at: "" } },
    ]);
    render(<InstalledPane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: "Apply now" }));
    await waitFor(() => expect(applyMarketplaceNow).toHaveBeenCalledWith(undefined));
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "success" }));
    expect(useAppState.getState().toasts[0].message).toContain("1 running project");
  });

  it("applies now with no running projects and shows an info toast", async () => {
    applyMarketplaceNow.mockResolvedValue([]);
    render(<InstalledPane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: "Apply now" }));
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "info" }));
  });

  it("F4: does not toast per-project sync errors from apply now (the event listener owns that)", async () => {
    applyMarketplaceNow.mockResolvedValue([
      {
        project_id: "p1",
        report: { installed: [], updated: [], removed: [], skipped: [], errors: ["boom"], finished_at: "" },
      },
    ]);
    render(<InstalledPane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: "Apply now" }));
    await waitFor(() => expect(applyMarketplaceNow).toHaveBeenCalled());
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "success" }));
    expect(useAppState.getState().toasts).toHaveLength(1);
  });

  it("toasts an error only when the apply-now call itself fails", async () => {
    applyMarketplaceNow.mockRejectedValue("container unreachable");
    render(<InstalledPane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: "Apply now" }));
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "error" }));
  });
});
