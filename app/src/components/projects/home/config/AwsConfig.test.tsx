import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, cleanup, render, screen, fireEvent, waitFor, within } from "@testing-library/react";
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
const awsCostEstimate = vi.fn(async (..._args: unknown[]) => null as unknown);
const awsEnvironmentMaxHours = vi.fn(async (_id: string) => 0 as number | null);

vi.mock("../../../../lib/tauri-commands", () => ({
  getAuthBridgeStatus: vi.fn(async () => OFF_BRIDGE),
  setAuthBridgeEnabled: vi.fn(),
  inspectCaCertPath: vi.fn(async () => null),
  removeAwsResources: (id: string, discard: boolean) => removeAwsResources(id, discard),
  awsCostEstimate: (...args: unknown[]) => awsCostEstimate(...args),
  awsEnvironmentMaxHours: (id: string) => awsEnvironmentMaxHours(id),
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

  it("still offers UDP mappings for a local project", () => {
    const project = localProject();
    project.port_mappings = [{ host_port: 5353, container_port: 5353, protocol: "udp" }];
    render(<AccessSection project={project} save={save} disabled={false} />);
    expect(within(screen.getByLabelText("Protocol 1")).getByText("UDP")).toBeInTheDocument();
    expect(screen.queryByText(/Reachable only from this machine/)).toBeNull();
  });

  it("leaves those options alone for a local project", () => {
    render(<RuntimeSection project={localProject()} save={save} disabled={false} />);
    expect(toggle("VPN support")).not.toBeDisabled();
    expect(toggle("Sandbox mode")).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByText("Not available on AWS (Fargate)")).toBeNull();
  });

  it("offers TCP port mappings on AWS, localhost only, and replaces folders", () => {
    const project = awsProject();
    project.port_mappings = [{ host_port: 3000, container_port: 3000, protocol: "tcp" }];
    render(<AccessSection project={project} save={save} disabled={false} />);
    expect(screen.getByText(/Reachable only from this machine \(localhost\)/)).toBeInTheDocument();
    expect(screen.queryByText(/Expose container ports/)).toBeNull();
    const protocol = screen.getByLabelText("Protocol 1");
    expect(within(protocol).queryByText("UDP")).toBeNull();
    expect(within(protocol).getByText("TCP")).toBeInTheDocument();
    cleanup();

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

describe("LLM gateway sidecar (ModelSection)", () => {
  const SIDECAR = "Run Triple-C's LLM gateway in the workspace";
  const withModels = (models: { name: string; model_id: string }[]) =>
    useAppState.setState({
      appSettings: { gateway: { enabled: false, port: 4000, models } } as unknown as AppSettings,
    });
  const oai = (over: Partial<Project> = {}) =>
    awsProject({
      backend: "open_ai_compatible",
      openai_compatible_config: {
        base_url: "http://host.docker.internal:4000",
        api_key: null,
        model_id: "gpt",
        haiku_model_id: null,
      } as never,
      ...over,
    });
  const renderModel = (p: Project) =>
    render(<ModelSection project={p} save={save} disabled={false} />);

  it("is offered only for an AWS project on the OpenAI-compatible backend", () => {
    withModels([{ name: "gpt", model_id: "gpt-5" }]);
    renderModel(awsProject({ backend: "ollama" }));
    expect(screen.queryByRole("switch", { name: SIDECAR })).toBeNull();
    cleanup();
    render(
      <ModelSection
        project={localProjectOai()}
        save={save}
        disabled={false}
      />,
    );
    expect(screen.queryByRole("switch", { name: SIDECAR })).toBeNull();
    useAppState.setState({ appSettings: null });
  });

  it("turns on when Settings has a gateway model, and says why not otherwise", async () => {
    withModels([{ name: " ", model_id: "gpt-5" }]);
    renderModel(oai());
    expect(toggle(SIDECAR)).toBeDisabled();
    expect(screen.getByRole("alert")).toHaveTextContent("Settings → Model Gateway");
    cleanup();

    withModels([{ name: "gpt", model_id: "gpt-5" }]);
    renderModel(oai());
    expect(toggle(SIDECAR)).not.toBeDisabled();
    fireEvent.click(toggle(SIDECAR));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0].aws.gateway_sidecar).toBe(true);
    useAppState.setState({ appSettings: null });
  });

  it("needs 1 vCPU and 2 GiB", () => {
    withModels([{ name: "gpt", model_id: "gpt-5" }]);
    const p = oai();
    p.aws = { ...p.aws!, cpu: 512, memory_mib: 1024 };
    renderModel(p);
    expect(toggle(SIDECAR)).toBeDisabled();
    expect(screen.getByRole("alert")).toHaveTextContent("1 vCPU and 2 GiB");
    useAppState.setState({ appSettings: null });
  });

  it("when on, replaces the local-endpoint warning and can always be turned off", async () => {
    withModels([]);
    const p = oai();
    p.aws = { ...p.aws!, gateway_sidecar: true };
    renderModel(p);
    expect(screen.queryByText(/host.docker.internal is this machine/)).toBeNull();
    expect(screen.getByRole("note", { name: "LLM gateway sidecar" })).toHaveTextContent(
      "http://127.0.0.1:4000",
    );
    expect(toggle(SIDECAR)).toHaveAttribute("aria-checked", "true");
    expect(toggle(SIDECAR)).not.toBeDisabled();
    fireEvent.click(toggle(SIDECAR));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0].aws.gateway_sidecar).toBe(false);
    useAppState.setState({ appSettings: null });
  });

  it("is turned off with the backend it needs", async () => {
    withModels([{ name: "gpt", model_id: "gpt-5" }]);
    const p = oai();
    p.aws = { ...p.aws!, gateway_sidecar: true };
    renderModel(p);
    fireEvent.change(screen.getByLabelText("Backend"), { target: { value: "anthropic" } });
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0]).toMatchObject({ backend: "anthropic", aws: { gateway_sidecar: false } });
    useAppState.setState({ appSettings: null });
  });
});

function localProjectOai(): Project {
  return localProject({
    backend: "open_ai_compatible",
    openai_compatible_config: {
      base_url: "http://host.docker.internal:4000",
      api_key: null,
      model_id: null,
      haiku_model_id: null,
    } as never,
  });
}

describe("HostSection", () => {
  const renderHost = (p: Project = awsProject(), disabled = false) =>
    render(<HostSection project={p} save={save} disabled={disabled} />);

  it("shows the estimated hourly cost for the project's connection and region", async () => {
    awsCostEstimate.mockResolvedValueOnce({
      usd_per_hour: 0.05,
      note_spot: true,
      efs_billed_separately: true,
    });
    useAppState.setState({
      appSettings: { hosts: [{ id: "c1", name: "lab" }] } as unknown as AppSettings,
    });
    const p = awsProject();
    p.aws = { ...p.aws!, spot: true };
    renderHost(p);
    const card = screen.getByRole("region", { name: "Estimated cost" });
    await waitFor(() =>
      expect(within(card).getByText(/≈ \$0\.050\/hour \(on-demand; Spot is usually up to 70% cheaper\)/)).toBeInTheDocument(),
    );
    expect(awsCostEstimate).toHaveBeenCalledWith("c1", "eu-west-1", p.aws);
    expect(within(card).getByText(/EFS storage is billed separately/)).toBeInTheDocument();
    useAppState.setState({ appSettings: null });
  });

  it("says the estimate is unavailable when the connection is gone", async () => {
    useAppState.setState({ appSettings: null });
    renderHost();
    const card = screen.getByRole("region", { name: "Estimated cost" });
    await waitFor(() => expect(within(card).getByText("Estimate unavailable")).toBeInTheDocument());
    expect(awsCostEstimate).not.toHaveBeenCalled();
  });

  it("offers no CPU below 1 vCPU while the gateway sidecar is on", () => {
    const p = awsProject();
    p.aws = { ...p.aws!, gateway_sidecar: true };
    renderHost(p);
    const cpus = within(screen.getByLabelText("CPU"))
      .getAllByRole("option")
      .map((o) => (o as HTMLOptionElement).value);
    expect(cpus).not.toContain("256");
    expect(cpus).not.toContain("512");
    expect(cpus).toContain("1024");
  });

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
    expect(screen.queryByText(/coming later/)).toBeNull();
  });

  it("offers EFS storage with what it costs", async () => {
    const p = awsProject();
    renderHost(p);
    const efs = screen.getByRole("radio", { name: "EFS" });
    expect(efs).not.toBeDisabled();
    fireEvent.click(efs);
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0].aws.storage).toBe("efs");

    p.aws = { ...p.aws!, storage: "efs" };
    renderHost(p);
    expect(
      screen.getByText("Everything in /workspace survives a stop; git and builds are slower over NFS"),
    ).toBeInTheDocument();
  });

  it("chooses where the credentials live and warns what copy-at-connect means", async () => {
    const p = awsProject();
    renderHost(p);
    expect(screen.getByRole("radio", { name: "Stored in AWS (Secrets Manager)" })).toBeChecked();
    expect(screen.getByText(/kept in the project's Secrets Manager secret/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Copied when you connect" }));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0].aws.secrets_delivery).toBe("copy_at_connect");
    cleanup();

    p.aws = { ...p.aws!, secrets_delivery: "copy_at_connect", gateway_sidecar: false };
    renderHost(p);
    const note = screen.getByRole("note", { name: "Copied when you connect" });
    expect(note).toHaveTextContent(
      "Private repositories clone, and the setup script runs, after you connect (or after 5 minutes without).",
    );
    expect(note).toHaveTextContent(/Claude Code's self-update may fail until then/);
    expect(note).toHaveTextContent("SSH keys, a Git token or variables you remove from the project stay in a running workspace until it stops.");
    expect(note).toHaveTextContent(/not run again when a later connect re-clones/);
    expect(note).toHaveTextContent(
      "Scheduled jobs and the setup script never get the API keys or custom variables; Git and SSH work once the app has connected.",
    );
    expect(note).toHaveTextContent("~/.ssh is not kept on EFS");
    expect(note).toHaveTextContent("/mnt/triple-c-home/.ssh stay there until you remove them");
    expect(note).not.toHaveTextContent(/gateway/);
    expect(screen.getByText(/repositories and SSH keys included, starts fresh/)).toBeInTheDocument();
    cleanup();

    // Ephemeral: nothing is kept anyway, so no SSH note.
    renderHost({ ...p, aws: { ...p.aws!, storage: "ephemeral" } });
    expect(screen.getByRole("note", { name: "Copied when you connect" })).not.toHaveTextContent("~/.ssh");
    cleanup();

    p.aws = { ...p.aws!, gateway_sidecar: true };
    renderHost(p);
    expect(screen.getByRole("note", { name: "Copied when you connect" })).toHaveTextContent(
      "The LLM gateway's keys are still stored in AWS",
    );
  });

  it("saves the setup script on blur, and a blank one as none", async () => {
    renderHost();
    const box = screen.getByLabelText("Setup script");
    expect(box.tagName).toBe("TEXTAREA");
    expect(box.className).toContain("font-mono");
    expect(box.className).not.toContain("focus:outline-none");
    fireEvent.change(box, { target: { value: "sudo apt-get install -y jq\n" } });
    expect(screen.getByText(/27 of 32,768 bytes/)).toBeInTheDocument();
    fireEvent.blur(box);
    await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    expect(save.mock.calls[0][0].aws.setup_script).toBe("sudo apt-get install -y jq\n");
    cleanup();

    const p = awsProject();
    p.aws = { ...p.aws!, setup_script: "echo hi" };
    renderHost(p);
    const again = screen.getByLabelText("Setup script");
    expect(again).toHaveValue("echo hi");
    fireEvent.change(again, { target: { value: "  \n" } });
    fireEvent.blur(again);
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(save.mock.calls[1][0].aws.setup_script).toBeNull();
  });

  it("refuses a setup script over 32 KiB with the backend's message", () => {
    renderHost();
    const box = screen.getByLabelText("Setup script");
    // 16,385 two-byte characters: 32,770 UTF-8 bytes.
    fireEvent.change(box, { target: { value: "é".repeat(16_385) } });
    fireEvent.blur(box);
    expect(screen.getByRole("alert")).toHaveTextContent("The setup script is 32770 bytes; the limit is 32768");
    expect(box).toHaveAttribute("aria-invalid", "true");
    expect(save).not.toHaveBeenCalled();
  });

  it("says when and how the setup script runs", () => {
    renderHost();
    expect(screen.getByText(/Runs on every start, as claude, with bash -e/)).toBeInTheDocument();
    expect(screen.getByText(/once the repositories are cloned/)).toBeInTheDocument();
    expect(screen.getByText(/stopped after 30 minutes, and its output shows in the Overview/)).toBeInTheDocument();
    expect(screen.queryByText(/never gets the API keys or custom variables/)).toBeNull();
    cleanup();

    const p = awsProject();
    p.aws = { ...p.aws!, secrets_delivery: "copy_at_connect" };
    renderHost(p);
    expect(
      screen.getByText(/the setup script waits for the app's first connect \(at most 5 minutes\) and never gets the API keys or custom variables/),
    ).toBeInTheDocument();
  });

  it("shows the environment's lifetime cap when it has one", async () => {
    awsEnvironmentMaxHours.mockResolvedValueOnce(24);
    renderHost();
    expect(
      await screen.findByText("This environment stops workspaces after 24 h (advisory)."),
    ).toBeInTheDocument();
    expect(awsEnvironmentMaxHours).toHaveBeenCalledWith(awsProject().id);
    cleanup();

    for (const none of [0, null]) {
      awsEnvironmentMaxHours.mockResolvedValueOnce(none);
      renderHost();
      await waitFor(() => expect(awsEnvironmentMaxHours).toHaveBeenCalled());
      await act(async () => {});
      expect(screen.queryByText(/stops workspaces after/)).toBeNull();
      cleanup();
    }
  });

  it("does not let the credentials setting change while the workspace runs", () => {
    renderHost(awsProject(), true);
    expect(screen.getByRole("radio", { name: "Copied when you connect" })).toBeDisabled();
  });

  it("says /workspace files stay on their access point after switching away from EFS", () => {
    const p = awsProject();
    p.aws = {
      ...p.aws!,
      storage: "hybrid",
      workspace_access_point_id: "fsap-w",
      task_arn: null,
      task_definition_arn: null,
      access_point_id: null,
    };
    renderHost(p);
    expect(screen.getByRole("note", { name: "EFS files kept" })).toHaveTextContent(
      /\/workspace files .* stay on their access point until .*Remove AWS resources/,
    );
    // It is an AWS resource: removable, and it keeps the project in place.
    expect(screen.getByRole("button", { name: "Run on this machine instead" })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove AWS resources…" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("checkbox", { name: /access points/ })).toBeInTheDocument();
    expect(dialog).toHaveTextContent(/\/workspace/);
  });

  it("does not show the kept-files note on EFS or without a workspace access point", () => {
    const p = awsProject();
    p.aws = { ...p.aws!, storage: "efs", workspace_access_point_id: "fsap-w" };
    renderHost(p);
    expect(screen.queryByRole("note", { name: "EFS files kept" })).toBeNull();
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

  it("keeps a project whose kept files' access point remains on AWS until it is removed", () => {
    const p = awsProject();
    p.aws = { ...p.aws!, access_point_id: "fsap-1", task_arn: null, task_definition_arn: null };
    renderHost(p);
    const move = screen.getByRole("button", { name: "Run on this machine instead" });
    expect(move).toHaveAttribute("aria-disabled", "true");
    expect(move).toHaveAccessibleDescription(/access point is still on AWS/);
    expect(screen.getByRole("button", { name: "Remove AWS resources…" })).not.toHaveAttribute(
      "aria-disabled",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove AWS resources…" }));
    expect(within(screen.getByRole("dialog")).getByText(/access point\s+stays too/)).toBeInTheDocument();
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
