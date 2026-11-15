import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import RuntimeSection from "./RuntimeSection";
import AccessSection from "./AccessSection";
import WorkspaceSection from "./WorkspaceSection";
import ModelSection from "./ModelSection";
import HostSection from "./HostSection";
import { awsProject, localProject } from "../../../../test/awsFixtures";
import type { AppSettings, AuthBridgeStatus, Project } from "../../../../lib/types";
import { useAppState } from "../../../../store/appState";

const OFF_BRIDGE: AuthBridgeStatus = { enabled: false, active_ports: [], conflicts: [] };
const removeAwsResources = vi.fn();

vi.mock("../../../../lib/tauri-commands", () => ({
  getAuthBridgeStatus: vi.fn(async () => OFF_BRIDGE),
  setAuthBridgeEnabled: vi.fn(),
  inspectCaCertPath: vi.fn(async () => null),
  removeAwsResources: (id: string, discard: boolean) => removeAwsResources(id, discard),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

const save = vi.fn().mockResolvedValue(true);

beforeEach(() => vi.clearAllMocks());

function toggle(name: string) {
  return screen.getByRole("switch", { name });
}

describe("AWS project config", () => {
  it("locks Docker access, VPN and the sandbox on AWS, saying why", () => {
    render(<RuntimeSection project={awsProject()} save={save} disabled={false} />);
    for (const name of ["Allow container spawning", "VPN support", "Sandbox mode"]) {
      expect(toggle(name)).toBeDisabled();
      expect(toggle(name)).toHaveAttribute("aria-checked", "false");
    }
    expect(screen.getAllByText("Not available on AWS (Fargate)")).toHaveLength(2);
    expect(screen.getByText(/Off on AWS for now/)).toBeInTheDocument();
  });

  it("leaves those options alone for a local project", () => {
    render(<RuntimeSection project={localProject()} save={save} disabled={false} />);
    expect(toggle("VPN support")).not.toBeDisabled();
    expect(toggle("Sandbox mode")).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByText("Not available on AWS (Fargate)")).toBeNull();
  });

  it("replaces port mappings and folders on AWS", () => {
    render(<AccessSection project={awsProject()} save={save} disabled={false} />);
    expect(screen.getByText("Not available on AWS (Fargate)")).toBeInTheDocument();
    expect(screen.queryByText(/Expose container ports/)).toBeNull();

    render(<WorkspaceSection project={awsProject()} save={save} disabled={false} />);
    expect(screen.queryByText("Folders")).toBeNull();
    expect(screen.getByText(/mounts no folders/)).toBeInTheDocument();
  });

  it("allows every custom-endpoint backend on AWS with the reachability warning", () => {
    render(
      <ModelSection
        project={awsProject({
          backend: "open_ai_compatible",
          openai_compatible_config: { base_url: "https://llm.example.com", api_key: null, model_id: null, haiku_model_id: null } as never,
        })}
        save={save}
        disabled={false}
      />,
    );
    const oai = screen.getByRole("option", { name: "OpenAI Compatible" }) as HTMLOptionElement;
    expect(oai.disabled).toBe(false);
    expect(screen.getByText(/must be reachable from the AWS task/)).toBeInTheDocument();
    expect(screen.queryByText(/can't reach it/)).toBeNull();
  });

  it("warns when the endpoint is this machine, or the app's own gateway", () => {
    const { unmount } = render(
      <ModelSection project={awsProject({ backend: "ollama" })} save={save} disabled={false} />,
    );
    // The Ollama default is host.docker.internal.
    expect(screen.getByText(/host.docker.internal is this machine/)).toBeInTheDocument();
    unmount();
    useAppState.setState({
      appSettings: { gateway: { enabled: true, port: 4000 } } as unknown as AppSettings,
    });
    render(
      <ModelSection
        project={awsProject({
          backend: "open_ai_compatible",
          openai_compatible_config: { base_url: "http://127.0.0.1:4000", api_key: null, model_id: null, haiku_model_id: null } as never,
        })}
        save={save}
        disabled={false}
      />,
    );
    expect(screen.getByText(/Triple-C's own LLM gateway/)).toBeInTheDocument();
    useAppState.setState({ appSettings: null });
  });
});

describe("HostSection", () => {
  const renderHost = (p: Project = awsProject(), disabled = false) =>
    render(<HostSection project={p} save={save} disabled={disabled} />);

  it("saves a CPU change with a memory size valid for it", async () => {
    renderHost(); // 1024 CPU / 4096 MiB
    fireEvent.change(screen.getByLabelText("CPU"), { target: { value: "256" } });
    await waitFor(() => expect(save).toHaveBeenCalled());
    const aws = save.mock.calls[0][0].aws;
    expect([aws.cpu, aws.memory_mib]).toEqual([256, 2048]);
  });

  it("saves a valid memory size and refuses an invalid one", async () => {
    renderHost();
    const mem = screen.getByLabelText("Memory (MiB)");
    fireEvent.change(mem, { target: { value: "5000" } });
    fireEvent.blur(mem);
    expect(screen.getByRole("alert")).toHaveTextContent(/5000 MiB of memory is not valid with 1024/);
    expect(save).not.toHaveBeenCalled();

    fireEvent.change(mem, { target: { value: "8192" } });
    fireEvent.blur(mem);
    await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    expect(save.mock.calls[0][0].aws).toMatchObject({ cpu: 1024, memory_mib: 8192 });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("warns that Ephemeral storage loses the login and history", () => {
    const p = awsProject();
    p.aws = { ...p.aws!, storage: "ephemeral" };
    renderHost(p);
    expect(screen.getByText(/Claude login and history are lost when the workspace stops/)).toBeInTheDocument();
    expect(screen.getByText(/EFS .* coming later/)).toBeInTheDocument();
  });

  it("saves repository edits on blur", async () => {
    renderHost();
    fireEvent.click(screen.getByRole("button", { name: "+ Add repository" }));
    const url = screen.getByLabelText("Repository 2 URL");
    fireEvent.change(url, { target: { value: "git@github.com:org/web.git" } });
    fireEvent.blur(url);
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0].aws.repos).toEqual([
      { url: "https://example.com/org/api.git", branch: null, folder: "api" },
      { url: "git@github.com:org/web.git", branch: null, folder: "web" },
    ]);
  });

  it("removes AWS resources after a confirm, and only then allows moving to this machine", async () => {
    const p = awsProject();
    p.aws = { ...p.aws!, task_definition_arn: "arn:td:1" };
    removeAwsResources.mockResolvedValue({ ...p, aws: { ...p.aws, task_definition_arn: null } });
    renderHost(p);
    const move = screen.getByRole("button", { name: "Run on this machine instead" });
    expect(move).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(move);
    expect(save).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Remove AWS resources…" }));
    const dialog = screen.getByRole("dialog");
    // Hybrid: the access point can go too (the files stay on the user's EFS).
    fireEvent.click(within(dialog).getByRole("checkbox"));
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(removeAwsResources).toHaveBeenCalledWith(p.id, true));
  });

  it("keeps the AWS resource buttons locked while the project is in Error (its task may live)", () => {
    const p = awsProject({ status: "error" });
    p.aws = { ...p.aws!, task_arn: "arn:task" };
    renderHost(p, false);
    for (const name of ["Remove AWS resources…", "Run on this machine instead"]) {
      const b = screen.getByRole("button", { name });
      expect(b).toHaveAttribute("aria-disabled", "true");
      expect(b).toHaveAccessibleDescription(/still running/);
    }
  });

  it("offers no saved-files choice for an Ephemeral workspace", () => {
    const p = awsProject();
    p.aws = { ...p.aws!, storage: "ephemeral", task_definition_arn: "arn:td:1" };
    renderHost(p);
    fireEvent.click(screen.getByRole("button", { name: "Remove AWS resources…" }));
    expect(within(screen.getByRole("dialog")).queryByRole("checkbox")).toBeNull();
  });

  it("only promises the Spot push when the WIP push is on", () => {
    const p = awsProject();
    p.aws = { ...p.aws!, wip_push_on_stop: false };
    renderHost(p);
    expect(screen.getByText(/unpushed work is lost/)).toBeInTheDocument();
    expect(screen.queryByText(/push below still runs/)).toBeNull();
  });

  it("keeps a repository being typed when the project list refreshes with the same repos", () => {
    const p = awsProject();
    const { rerender } = renderHost(p);
    fireEvent.click(screen.getByRole("button", { name: "+ Add repository" }));
    fireEvent.change(screen.getByLabelText("Repository 2 URL"), { target: { value: "git@h:o/half" } });
    // A refresh hands over a new, equal array.
    const refreshed = { ...p, aws: { ...p.aws!, repos: p.aws!.repos.map((r) => ({ ...r })) } };
    rerender(<HostSection project={refreshed} save={save} disabled={false} />);
    expect(screen.getByLabelText("Repository 2 URL")).toHaveValue("git@h:o/half");
    // A real change replaces the rows.
    const changed = { ...p, aws: { ...p.aws!, repos: [] } };
    rerender(<HostSection project={changed} save={save} disabled={false} />);
    expect(screen.queryByLabelText("Repository 1 URL")).toBeNull();
  });
});
