import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import RuntimeSection from "./RuntimeSection";
import AccessSection from "./AccessSection";
import WorkspaceSection from "./WorkspaceSection";
import ModelSection from "./ModelSection";
import HostSection from "./HostSection";
import { awsProject, localProject } from "../../../../test/awsFixtures";
import type { AuthBridgeStatus, Project } from "../../../../lib/types";

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

  it("disables the gateway backend and warns about local model URLs on AWS", () => {
    render(<ModelSection project={awsProject({ backend: "ollama" })} save={save} disabled={false} />);
    const gateway = screen.getByRole("option", { name: /OpenAI Compatible/ }) as HTMLOptionElement;
    expect(gateway.disabled).toBe(true);
    expect(gateway.textContent).toMatch(/Not available on AWS/);
    expect(screen.getByText(/must be reachable from the AWS task/)).toBeInTheDocument();
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
    fireEvent.click(within(dialog).getByRole("checkbox"));
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(removeAwsResources).toHaveBeenCalledWith(p.id, true));
  });
});
