import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import EnvironmentsPanel from "./EnvironmentsPanel";
import type { DeployProgress, EnvironmentScan } from "../../../lib/types";
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

/** Mock the backend: `detect` answers detection; `rest` answers everything else. */
function backend(detect: () => EnvironmentScan, rest: (cmd: string, args: unknown) => unknown = () => {
  throw new Error("unexpected");
}) {
  invoke.mockImplementation(async (cmd: string, args: unknown) => {
    if (cmd === "detect_aws_environments") return detect();
    return rest(cmd, args);
  });
}

const detectCalls = () => invoke.mock.calls.filter((c) => c[0] === "detect_aws_environments").length;

async function open() {
  fireEvent.click(screen.getByRole("button", { name: "Environments" }));
  await waitFor(() => expect(detectCalls()).toBeGreaterThan(0));
}

describe("EnvironmentsPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    handlers.clear();
  });

  it("detects in the connection's default region when opened, and again on a region change", async () => {
    backend(() => scan());
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    expect(detectCalls()).toBe(0);
    await open();
    expect(invoke).toHaveBeenCalledWith("detect_aws_environments", { connectionId: "h1", region: "eu-west-1" });
    fireEvent.change(screen.getByLabelText("Region"), { target: { value: "us-east-1" } });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("detect_aws_environments", { connectionId: "h1", region: "us-east-1" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Detect again" }));
    await waitFor(() => expect(detectCalls()).toBe(3));
  });

  it("shows deploy as primary when nothing is found and the user is an admin", async () => {
    backend(() => scan());
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const deploy = await screen.findByRole("button", { name: "Deploy a new environment" });
    expect(deploy.className).toContain("--accent-emphasis");
    expect(screen.getByText(/No Triple-C environment in eu-west-1/)).toBeInTheDocument();
    fireEvent.click(deploy);
    expect(screen.getByRole("dialog", { name: "Deploy a new environment" })).toBeInTheDocument();
  });

  it("deploy is secondary when environments exist", async () => {
    backend(() => scan({ environments: [env({ onboarding: { state: "joined" }, actions: [] })] }));
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const deploy = await screen.findByRole("button", { name: "Deploy a new environment" });
    expect(deploy.className).not.toContain("--accent-emphasis");
    expect(within(screen.getByTestId("env-dev")).getByText("Joined")).toBeInTheDocument();
  });

  it("offers an admin bundle instead of deploy for a workspace user with nothing found", async () => {
    backend(
      () => scan(),
      (cmd) => {
        if (cmd === "aws_policy_document") return [{ name: "environment-admin.json", json: "{}" }];
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="workspace_user" />);
    await open();
    expect(await screen.findByText(/needs an environment admin/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Deploy a new environment" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show the environment-admin policies" }));
    expect(await screen.findByText("environment-admin.json")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("aws_policy_document", { tier: "environment_admin" });
    fireEvent.click(screen.getByRole("button", { name: "Request an environment from an admin…" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("button", { name: "Save bundle to folder…" })).toBeInTheDocument();
  });

  it("treats a connection with no permission report as not an admin", async () => {
    backend(() => scan());
    render(<EnvironmentsPanel connection={conn} />);
    await open();
    expect(await screen.findByRole("button", { name: "Request an environment from an admin…" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Deploy a new environment" })).toBeNull();
  });

  it("an incomplete environment offers repair and destroy, destroy only after typing its name", async () => {
    backend(
      () => scan({ environments: [env({ state: { state: "incomplete", lock: null }, actions: ["repair", "destroy"], info: null, onboarding: null, template_version: null })] }),
      (cmd) => {
        if (cmd === "destroy_incomplete_aws_environment") return "op9";
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const row = await screen.findByTestId("env-dev");
    expect(within(row).getByText("Incomplete")).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Repair dev" })).toBeInTheDocument();

    // Repair without a record asks for the network and warns about a change.
    fireEvent.click(within(row).getByRole("button", { name: "Repair dev" }));
    const repair = screen.getByRole("dialog", { name: "Repair dev" });
    expect(within(repair).getByRole("radio", { name: "Public" })).toHaveAttribute("aria-checked", "true");
    expect(within(repair).getByText(/different network than the one it was deployed with replaces/)).toBeInTheDocument();
    fireEvent.click(within(repair).getByRole("button", { name: "Cancel" }));

    fireEvent.click(within(row).getByRole("button", { name: "Destroy dev" }));
    const dialog = screen.getByRole("dialog", { name: "Destroy dev" });
    expect(within(dialog).getByText(/deployment is incomplete \(no valid environment record\)/)).toBeInTheDocument();
    const confirm = within(dialog).getByRole("button", { name: "Destroy" });
    expect(confirm).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText(/Type dev to confirm/), { target: { value: "de" } });
    expect(confirm).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText(/Type dev to confirm/), { target: { value: "dev" } });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("destroy_incomplete_aws_environment", {
        connectionId: "h1",
        region: "eu-west-1",
        name: "dev",
      }),
    );
  });

  it("a stale lock offers unlock with its owner and age", async () => {
    const lock = { who: "alice@laptop", created: "2026-11-14T07:00:00Z", operation: "OperationTypeApply", age_secs: 3 * 3600 + 300, local_process: false };
    backend(
      () => scan({ environments: [env({ state: { state: "incomplete", lock }, actions: ["repair", "unlock"], onboarding: null })] }),
      (cmd) => {
        if (cmd === "unlock_aws_state") return undefined;
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const row = await screen.findByTestId("env-dev");
    expect(within(row).getByText(/Locked by alice@laptop/)).toBeInTheDocument();
    expect(within(row).getByText(/3 h 5 min ago/)).toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: "Destroy dev" })).toBeNull();

    fireEvent.click(within(row).getByRole("button", { name: "Unlock dev" }));
    const dialog = screen.getByRole("dialog", { name: "Unlock dev" });
    expect(within(dialog).getByText(/alice@laptop/)).toBeInTheDocument();
    expect(within(dialog).getByText(/3 h 5 min ago/)).toBeInTheDocument();
    const confirm = within(dialog).getByRole("button", { name: "Unlock" });
    expect(confirm).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText(/Type dev to confirm/), { target: { value: "dev" } });
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("unlock_aws_state", {
        connectionId: "h1",
        region: "eu-west-1",
        stateKey: "environments/dev/terraform.tfstate",
      }),
    );
    await waitFor(() => expect(detectCalls()).toBe(2));
  });

  it("shows the server's refusal when a lock is still held", async () => {
    backend(
      () => scan({ environments: [env({ state: { state: "incomplete", lock: null }, actions: ["repair", "destroy"], onboarding: null })] }),
      (cmd) => {
        if (cmd === "repair_aws_environment") throw { kind: "other", message: "The state is locked; Unlock first" };
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    // With a record, Repair runs straight away on the recorded network.
    fireEvent.click(await screen.findByRole("button", { name: "Repair dev" }));
    expect(await screen.findByText("The state is locked; Unlock first")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("repair_aws_environment", { connectionId: "h1", region: "eu-west-1", name: "dev", network: null });
  });

  it("join onboards an admin and shows progress until done, then re-detects", async () => {
    const key = "users/dev/abc123abc123/terraform.tfstate";
    let detects = 0;
    backend(
      () => {
        detects += 1;
        return scan({ environments: [env({ onboarding: detects > 1 ? { state: "joined" } : { state: "not_joined" } })] });
      },
      async (cmd) => {
        if (cmd === "onboard_aws_user") {
          // The backend can emit before the invoke resolves: an old follower's
          // line (another op) and this run's first line.
          emit(progress({ op_id: "old", state_key: key, events: [tofuEvent("stale line")] }));
          emit(progress({ op_id: "op1", state_key: key, events: [tofuEvent("Planning")], cancellable: false }));
          return "op1";
        }
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    fireEvent.click(await screen.findByRole("button", { name: "Join dev" }));
    expect(await screen.findByText("Planning")).toBeInTheDocument();
    expect(screen.queryByText("stale line")).toBeNull();
    expect(invoke).toHaveBeenCalledWith("onboard_aws_user", { connectionId: "h1", region: "eu-west-1", environment: "dev" });
    expect(screen.getByRole("button", { name: "Cancel deployment" })).toBeDisabled();

    emit(progress({ op_id: "op1", state_key: key, events: [tofuEvent("Creating...", { resource: "aws_iam_role.task", action: "create" })] }));
    expect(screen.getByText("aws_iam_role.task")).toBeInTheDocument();
    emit(progress({ op_id: "other", state_key: "environments/x/terraform.tfstate", events: [tofuEvent("unrelated")] }));
    expect(screen.queryByText("unrelated")).toBeNull();
    expect(detectCalls()).toBe(1);

    emit(progress({ op_id: "op1", state_key: key, events: [], done: true, cancellable: false, result: { ok: true, exit_code: 0, summary: "Apply complete! Resources: 4 added" } }));
    expect(await screen.findByText(/Apply complete! Resources: 4 added/)).toBeInTheDocument();
    await waitFor(() => expect(detectCalls()).toBe(2));
    expect(await within(screen.getByTestId("env-dev")).findByText("Joined")).toBeInTheDocument();
  });

  it("a workspace user's join opens the onboarding bundle and waits for an admin", async () => {
    backend(
      () => scan({ environments: [env()] }),
      (cmd) => {
        if (cmd === "save_aws_admin_bundle") return "/home/me/onboard-dev";
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="workspace_user" />);
    await open();
    fireEvent.click(await screen.findByRole("button", { name: "Join dev" }));
    const dialog = screen.getByRole("dialog");
    // Opening the bundle is not a request yet.
    expect(screen.queryByText("Waiting for an admin to onboard you")).toBeNull();
    fireEvent.click(within(dialog).getByRole("button", { name: "Save bundle to folder…" }));
    expect(await within(dialog).findByText("/home/me/onboard-dev")).toBeInTheDocument();
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("save_aws_admin_bundle", {
        connectionId: "h1",
        region: "eu-west-1",
        kind: "user_onboard",
        environment: "dev",
        ownerId: "AIDAEXAMPLEEXAMPLE01",
        network: null,
      }),
    );
    expect(invoke.mock.calls.some((c) => c[0] === "onboard_aws_user")).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(within(screen.getByTestId("env-dev")).getByText("Waiting for an admin to onboard you")).toBeInTheDocument();
  });

  it("follows a deployment running on this machine when opened, and cancels after confirming", async () => {
    const lock = { who: "me@here", created: "x", operation: "OperationTypeApply", age_secs: 60, local_process: true };
    backend(
      () => scan({ environments: [env({ state: { state: "deploying", lock }, actions: ["follow"], onboarding: null })] }),
      async (cmd) => {
        if (cmd === "follow_aws_deployment") {
          emit(progress({ op_id: "f1", events: [tofuEvent("replayed line")] }));
          return "f1";
        }
        if (cmd === "cancel_aws_deployment") return undefined;
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    expect(await screen.findByText("replayed line")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("follow_aws_deployment", {
      connectionId: "h1",
      region: "eu-west-1",
      stateKey: "environments/dev/terraform.tfstate",
    });
    fireEvent.click(screen.getByRole("button", { name: "Cancel deployment" }));
    const dialog = screen.getByRole("dialog", { name: "Cancel this deployment?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel deployment" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("cancel_aws_deployment", {
        connectionId: "h1",
        region: "eu-west-1",
        stateKey: "environments/dev/terraform.tfstate",
      }),
    );
  });

  it("shows a failed run's errors in full", async () => {
    backend(
      () => scan({ environments: [env({ state: { state: "update_available" }, actions: ["join", "update"], onboarding: { state: "joined" } })] }),
      async (cmd) => {
        if (cmd === "update_aws_environment") return "u1";
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    fireEvent.click(await screen.findByRole("button", { name: "Update dev" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("update_aws_environment", { connectionId: "h1", region: "eu-west-1", name: "dev" }));
    const long = "Error: creating IAM Role (triple-c-dev-task): AccessDenied: User is not authorized to perform iam:CreateRole\n  with aws_iam_role.task";
    emit(progress({ op_id: "u1", events: [tofuEvent(long, { level: "error", kind: "diagnostic" })] }));
    emit(progress({ op_id: "u1", events: [], done: true, cancellable: false, result: { ok: false, exit_code: 1, summary: "Apply failed" } }));
    expect(await screen.findByText(/Failed \(exit 1\): Apply failed/)).toBeInTheDocument();
    expect(screen.getByText((_, el) => el?.textContent === long && el.tagName === "PRE")).toBeInTheDocument();
  });

  it("a workspace user who closes the bundle without saving is not waiting", async () => {
    backend(() => scan({ environments: [env()] }));
    render(<EnvironmentsPanel connection={conn} tier="workspace_user" />);
    await open();
    fireEvent.click(await screen.findByRole("button", { name: "Join dev" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Close" }));
    expect(screen.queryByText("Waiting for an admin to onboard you")).toBeNull();
    expect(within(screen.getByTestId("env-dev")).getByRole("button", { name: "Join dev" })).toHaveTextContent("Join");
  });

  it("never offers Destroy on a ready environment", async () => {
    backend(() => scan({ environments: [env({ actions: ["join", "update"] })] }));
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const row = await screen.findByTestId("env-dev");
    expect(within(row).getByText("Ready")).toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: "Destroy dev" })).toBeNull();
    expect(within(row).queryByRole("button", { name: "Unlock dev" })).toBeNull();
  });

  it("after a region change, the old region's rows can't be acted on, and actions use the new region", async () => {
    let resolveUs: (s: EnvironmentScan) => void = () => {};
    invoke.mockImplementation(async (cmd: string, args: { region?: string }) => {
      if (cmd === "detect_aws_environments") {
        if (args.region === "us-east-1") return new Promise<EnvironmentScan>((r) => (resolveUs = r));
        return scan({ environments: [env({ state: { state: "incomplete", lock: null }, actions: ["repair", "destroy"], info: null, onboarding: null })] });
      }
      if (cmd === "update_aws_environment") return "u1";
      throw new Error(`unexpected ${cmd}`);
    });
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    expect(await screen.findByRole("button", { name: "Destroy dev" })).toBeEnabled();
    fireEvent.change(screen.getByLabelText("Region"), { target: { value: "us-east-1" } });
    // Before the new scan answers, nothing from eu-west-1 can be clicked.
    expect(screen.queryByTestId("env-dev")).toBeNull();
    expect(screen.queryByRole("button", { name: "Destroy dev" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Deploy a new environment" })).toBeNull();
    await act(async () =>
      resolveUs(scan({ region: "us-east-1", environments: [env({ state: { state: "update_available" }, actions: ["update"], onboarding: { state: "joined" } })] })),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Update dev" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("update_aws_environment", { connectionId: "h1", region: "us-east-1", name: "dev" }),
    );
    expect(await screen.findByText("Running")).toBeInTheDocument();
  });

  it("disables row actions while detection is in flight", async () => {
    let calls = 0;
    let release: () => void = () => {};
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd !== "detect_aws_environments") throw new Error(`unexpected ${cmd}`);
      calls += 1;
      const found = scan({ environments: [env({ actions: ["join", "update"], onboarding: { state: "joined" } })] });
      if (calls === 1) return found;
      return new Promise<EnvironmentScan>((r) => (release = () => r(found)));
    });
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    expect(await screen.findByRole("button", { name: "Update dev" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Detect again" }));
    expect(screen.getByRole("button", { name: "Update dev" })).toBeDisabled();
    await act(async () => release());
    expect(screen.getByRole("button", { name: "Update dev" })).toBeEnabled();
  });

  it("a done that arrives before the invoke resolves is shown and re-detects", async () => {
    backend(
      () => scan({ environments: [env({ state: { state: "update_available" }, actions: ["update"], onboarding: { state: "joined" } })] }),
      async (cmd) => {
        if (cmd === "update_aws_environment") {
          emit(progress({ op_id: "u1", events: [tofuEvent("Refreshing state")] }));
          emit(progress({ op_id: "u1", events: [], done: true, cancellable: false, result: { ok: true, exit_code: 0, summary: "No changes." } }));
          return "u1";
        }
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    fireEvent.click(await screen.findByRole("button", { name: "Update dev" }));
    expect(await screen.findByText(/Finished: No changes\./)).toBeInTheDocument();
    expect(screen.getByText("Refreshing state")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel deployment" })).toBeNull();
    await waitFor(() => expect(detectCalls()).toBe(2));
    expect(handlers.size).toBe(0);
  });

  it("routes a detection failure through the AWS error copy", async () => {
    backend(() => {
      throw { kind: "state_bucket_taken", bucket: "triple-c-tfstate-123456789012-eu-west-1" };
    });
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    expect(
      await screen.findByText("The state bucket name triple-c-tfstate-123456789012-eu-west-1 is already taken by another AWS account."),
    ).toBeInTheDocument();
  });
  it("explains an environment managed elsewhere and an incomplete one", async () => {
    backend(() =>
      scan({
        environments: [
          env({ name: "shared", state: { state: "externally_managed" }, actions: ["join"] }),
          env({ name: "half", state: { state: "incomplete", lock: null }, actions: ["repair", "destroy"], info: null, onboarding: null }),
        ],
      }),
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const shared = await screen.findByTestId("env-shared");
    expect(within(shared).getByText("Managed outside this account's Triple-C state")).toBeInTheDocument();
    expect(within(shared).getByText(/can't be updated from here/)).toBeInTheDocument();
    const half = screen.getByTestId("env-half");
    expect(within(half).getByText(/The last change didn't finish/)).toBeInTheDocument();
  });

  it("disables Repair and Destroy while a lock is held, saying Unlock first", async () => {
    const lock = { who: "bob@desk", created: "x", operation: "OperationTypeApply", age_secs: 3 * 3600, local_process: false };
    backend(() =>
      scan({ environments: [env({ state: { state: "incomplete", lock }, actions: ["repair", "destroy", "unlock"], info: null, onboarding: null })] }),
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const row = await screen.findByTestId("env-dev");
    expect(within(row).getByRole("button", { name: "Repair dev" })).toBeDisabled();
    expect(within(row).getByRole("button", { name: "Destroy dev" })).toBeDisabled();
    expect(within(row).getByRole("button", { name: "Unlock dev" })).toBeEnabled();
    expect(within(row).getByText("Unlock first")).toBeInTheDocument();
  });

  it("an incomplete onboarding keeps Join and offers its own Unlock", async () => {
    const lock = { who: "me@old", created: "x", operation: "OperationTypeApply", age_secs: 3 * 3600, local_process: false };
    backend(
      () =>
        scan({
          environments: [env({ onboarding: { state: "incomplete", lock }, onboarding_actions: ["unlock"] })],
        }),
      (cmd) => {
        if (cmd === "unlock_aws_state") return undefined;
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const row = await screen.findByTestId("env-dev");
    expect(within(row).getByText("Onboarding incomplete")).toBeInTheDocument();
    expect(within(row).getByText(/Your last onboarding didn't finish/)).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Join dev" })).toBeDisabled();
    fireEvent.click(within(row).getByRole("button", { name: "Unlock my onboarding to dev" }));
    const dialog = screen.getByRole("dialog", { name: "Unlock your onboarding to dev" });
    expect(within(dialog).getByText(/me@old/)).toBeInTheDocument();
    fireEvent.change(within(dialog).getByLabelText(/Type dev to confirm/), { target: { value: "dev" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Unlock" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("unlock_aws_state", {
        connectionId: "h1",
        region: "eu-west-1",
        stateKey: "users/dev/abc123abc123/terraform.tfstate",
      }),
    );
  });

  it("pushes a saved state back from the environment and the onboarding rows", async () => {
    backend(
      () =>
        scan({
          environments: [
            env({ name: "a", state: { state: "incomplete", lock: null }, actions: ["push_state"], info: null, onboarding: null }),
            env({ name: "b", onboarding: { state: "incomplete", lock: null }, onboarding_actions: ["push_state"] }),
          ],
        }),
      (cmd) => {
        if (cmd === "push_aws_saved_state") return "p1";
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    const a = await screen.findByTestId("env-a");
    expect(within(a).getByText(/couldn't save its state/)).toBeInTheDocument();
    fireEvent.click(within(a).getByRole("button", { name: "Push saved state of a" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("push_aws_saved_state", {
        connectionId: "h1",
        region: "eu-west-1",
        stateKey: "environments/a/terraform.tfstate",
      }),
    );
    const b = screen.getByTestId("env-b");
    // Join would only be refused until the saved state is back.
    expect(within(b).getByRole("button", { name: "Join b" })).toBeDisabled();
    fireEvent.click(within(b).getByRole("button", { name: "Push saved state of my onboarding to b" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("push_aws_saved_state", {
        connectionId: "h1",
        region: "eu-west-1",
        stateKey: "users/b/abc123abc123/terraform.tfstate",
      }),
    );
  });

  it("follows an onboarding running on this machine", async () => {
    backend(
      () =>
        scan({
          environments: [env({ onboarding: { state: "in_progress", lock: null }, onboarding_actions: ["follow"] })],
        }),
      async (cmd) => {
        if (cmd === "follow_aws_deployment") {
          emit(progress({ op_id: "f2", state_key: "users/dev/abc123abc123/terraform.tfstate", events: [tofuEvent("joining line")] }));
          return "f2";
        }
        throw new Error(`unexpected ${cmd}`);
      },
    );
    render(<EnvironmentsPanel connection={conn} tier="environment_admin" />);
    await open();
    expect(await screen.findByText("joining line")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("follow_aws_deployment", {
      connectionId: "h1",
      region: "eu-west-1",
      stateKey: "users/dev/abc123abc123/terraform.tfstate",
    });
    expect(within(screen.getByTestId("env-dev")).getByRole("button", { name: "Follow my onboarding to dev" })).toBeInTheDocument();
  });
});
