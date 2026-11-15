import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import DeployEnvironmentModal from "./DeployEnvironmentModal";
import type { DeployProgress } from "../../../lib/types";
import { conn, env, progress, scan, tofuEvent } from "./testkit";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));

type Handler = (e: { payload: DeployProgress }) => void;
const handlers = new Set<Handler>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: Handler) => {
    handlers.add(handler);
    return () => handlers.delete(handler);
  }),
}));
function emit(p: DeployProgress) {
  act(() => {
    for (const h of [...handlers]) h({ payload: p });
  });
}

const BOUNDARY_KEY = "boundary/terraform.tfstate";

function renderModal(over: Partial<Parameters<typeof DeployEnvironmentModal>[0]> = {}) {
  const props = {
    connectionId: conn.id,
    region: "eu-west-1",
    scan: scan(),
    onClose: vi.fn(),
    onDeploy: vi.fn().mockResolvedValue(null),
    onDetect: vi.fn(),
    ...over,
  };
  render(<DeployEnvironmentModal {...props} />);
  return props;
}

describe("DeployEnvironmentModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    handlers.clear();
  });

  it("the public network mode is the default and says it costs nothing", () => {
    const props = renderModal();
    expect(screen.getByRole("radio", { name: "Public" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByText("$0 — tasks get a public IP, no inbound access")).toBeInTheDocument();
    expect(screen.getByText("First deploy downloads OpenTofu (~30 MB) and the AWS provider (~150 MB).")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Private + NAT" }));
    expect(screen.getByText("about $33/month for the NAT gateway")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Public" }));
    expect(screen.getByLabelText("Name")).toHaveValue("main");
    fireEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(props.onDeploy).toHaveBeenCalledWith("main", { mode: "public" });
  });

  it("validates the name with the backend's rule", () => {
    const props = renderModal({ scan: scan({ environments: [env({ name: "main" })] }) });
    const deploy = screen.getByRole("button", { name: "Deploy" });
    expect(screen.getByText(/already exists/)).toBeInTheDocument();
    expect(deploy).toBeDisabled();
    for (const bad of ["Dev", "a--b", "dev-", "1dev", "a".repeat(21), ""]) {
      fireEvent.change(screen.getByLabelText("Name"), { target: { value: bad } });
      expect(deploy).toBeDisabled();
    }
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "team-2" } });
    expect(deploy).toBeEnabled();
    fireEvent.click(deploy);
    expect(props.onDeploy).toHaveBeenCalledWith("team-2", { mode: "public" });
  });

  it("an existing network needs a VPC and two distinct subnets", () => {
    const props = renderModal();
    fireEvent.click(screen.getByRole("radio", { name: "Existing VPC" }));
    const deploy = screen.getByRole("button", { name: "Deploy" });
    expect(deploy).toBeDisabled();
    fireEvent.change(screen.getByLabelText("VPC ID"), { target: { value: "vpc-0123abcd" } });
    fireEvent.change(screen.getByLabelText("Subnet ID 1"), { target: { value: "subnet-0123abcd" } });
    fireEvent.change(screen.getByLabelText("Subnet ID 2"), { target: { value: "subnet-0123abcd" } });
    expect(deploy).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Subnet ID 2"), { target: { value: "subnet-4567abcd" } });
    fireEvent.click(screen.getByRole("switch", { name: "Assign a public IP" }));
    fireEvent.click(deploy);
    expect(props.onDeploy).toHaveBeenCalledWith("main", {
      mode: "existing",
      vpc_id: "vpc-0123abcd",
      subnet_ids: ["subnet-0123abcd", "subnet-4567abcd"],
      assign_public_ip: true,
    });
  });

  it("keeps the dialog open and shows the server's refusal", async () => {
    const props = renderModal({
      onDeploy: vi.fn().mockResolvedValue({ kind: "other", message: "A deployment is starting for environment \"main\"; wait for it" }),
    });
    fireEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText(/A deployment is starting/)).toBeInTheDocument();
    expect(props.onClose).not.toHaveBeenCalled();
  });

  it("deploy modal blocks on a missing boundary and offers to create it", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "deploy_aws_permissions_boundary") return "b1";
      throw new Error(`unexpected ${cmd}`);
    });
    const props = renderModal({ scan: scan({ boundary: "no" }) });
    expect(screen.queryByLabelText("Name")).toBeNull();
    expect(screen.queryByRole("button", { name: "Deploy" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Create the permissions boundary (needs an account administrator)" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("deploy_aws_permissions_boundary", { connectionId: "h1", region: "eu-west-1" }),
    );
    emit(progress({ op_id: "b1", state_key: BOUNDARY_KEY, events: [tofuEvent("Creating boundary")] }));
    expect(screen.getByText("Creating boundary")).toBeInTheDocument();
    emit(progress({ op_id: "b1", state_key: BOUNDARY_KEY, done: true, cancellable: false, result: { ok: true, exit_code: 0, summary: "Apply complete!" } }));
    expect(await screen.findByLabelText("Name")).toBeInTheDocument();
    expect(props.onDetect).toHaveBeenCalled();
  });

  it("opens the boundary bundle when creating the boundary is denied", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "deploy_aws_permissions_boundary") throw { kind: "access_denied", action: "iam:CreatePolicy", message: "denied iam:CreatePolicy" };
      if (cmd === "save_aws_admin_bundle") return "/tmp/b";
      throw new Error(`unexpected ${cmd}`);
    });
    renderModal({ scan: scan({ boundary: "no" }) });
    fireEvent.click(screen.getByRole("button", { name: "Create the permissions boundary (needs an account administrator)" }));
    const dialog = await screen.findByRole("dialog", { name: "Permissions boundary bundle" });
    expect(within(dialog).getByText(/denied iam:CreatePolicy/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Save bundle to folder…" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("save_aws_admin_bundle", {
        connectionId: "h1",
        region: "eu-west-1",
        kind: "permissions_boundary",
        environment: null,
        ownerId: null,
        network: null,
      }),
    );
  });

  it("repairs a named environment and warns that a different network replaces it", () => {
    const props = renderModal({ repair: "dev" });
    expect(screen.getByRole("dialog", { name: "Repair dev" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Name")).toBeNull();
    expect(screen.getByText(/different network than the one it was deployed with replaces/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Private + NAT" }));
    fireEvent.click(screen.getByRole("button", { name: "Repair" }));
    expect(props.onDeploy).toHaveBeenCalledWith("dev", { mode: "private-nat" });
  });
});
