import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import HostsSettings from "./HostsSettings";
import { useAppState } from "../../store/appState";
import type { AppSettings, HostConnection, PermissionReport } from "../../lib/types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));

const conn: HostConnection = {
  id: "h1",
  name: "Prod",
  kind: { type: "aws", auth: { method: "profile", name: "prod-sso" }, default_region: "eu-west-1" },
};
const keysConn: HostConnection = {
  id: "h2",
  name: "Keys box",
  kind: { type: "aws", auth: { method: "keys", keyring_ref: "r" }, default_region: "us-east-1" },
};
const identity = { account: "123456789012", arn: "arn:aws:iam::123456789012:user/me", user_id: "U" };

function report(over: Partial<PermissionReport>): PermissionReport {
  return {
    identity,
    region: "eu-west-1",
    method: "simulated",
    summary: "insufficient",
    tiers: [
      { tier: "workspace_user", results: [{ action: "ecs:RunTask", verdict: "denied", note: null }] },
      { tier: "environment_admin", results: [{ action: "iam:CreateRole", verdict: "denied", note: null }] },
    ],
    ...over,
  };
}

function setHosts(hosts: HostConnection[]) {
  useAppState.setState({ appSettings: { hosts } as unknown as AppSettings });
}

describe("HostsSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    setHosts([conn, keysConn]);
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return ["dev", "prod-sso"];
      if (cmd === "get_settings") return { hosts: [conn, keysConn] };
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("shows Local Docker and existing AWS connections", async () => {
    render(<HostsSettings />);
    await screen.findByRole("option", { name: "dev" });
    expect(screen.getByText("Local Docker")).toBeInTheDocument();
    expect(screen.getByText("Prod")).toBeInTheDocument();
    expect(screen.getByText("Profile: prod-sso")).toBeInTheDocument();
    expect(within(screen.getByTestId("host-h2")).getByText("Access keys")).toBeInTheDocument();
    expect(within(screen.getByTestId("host-h1")).getByText("eu-west-1")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /^Check/ })).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "Remove Local Docker" })).toBeNull();
  });

  it("adds a profile connection and shows the signed-in identity", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return ["dev", "prod-sso"];
      if (cmd === "add_aws_profile_connection") return conn;
      if (cmd === "aws_connection_identity") return identity;
      if (cmd === "get_settings") return { hosts: [conn] };
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    await screen.findByRole("option", { name: "dev" });
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Dev" } });
    fireEvent.change(screen.getByLabelText("Profile"), { target: { value: "dev" } });
    fireEvent.change(screen.getByLabelText("Default region"), { target: { value: "eu-west-1" } });
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    expect(await screen.findByText(`Signed in as ${identity.arn}`)).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("add_aws_profile_connection", {
      name: "Dev",
      profile: "dev",
      defaultRegion: "eu-west-1",
    });
  });

  it("offers Sign in when the profile's SSO session expired", async () => {
    let identityCalls = 0;
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return ["dev"];
      if (cmd === "add_aws_profile_connection") return conn;
      if (cmd === "aws_connection_identity") {
        if (identityCalls++ === 0) throw { kind: "sso_expired", profile: "dev" };
        return identity;
      }
      if (cmd === "aws_connection_sso_login") return undefined;
      if (cmd === "get_settings") return { hosts: [conn] };
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    await screen.findByRole("option", { name: "dev" });
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Dev" } });
    fireEvent.change(screen.getByLabelText("Profile"), { target: { value: "dev" } });
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    expect(await screen.findByText("Your SSO session for profile dev has expired.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText(`Signed in as ${identity.arn}`)).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("aws_connection_sso_login", { id: "h1" });
    // The connection was added once; only the failed step was retried.
    expect(invoke.mock.calls.filter((c) => c[0] === "add_aws_profile_connection")).toHaveLength(1);
  });

  it("access key inputs are password fields and the session token is optional", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "add_aws_keys_connection") return keysConn;
      if (cmd === "aws_connection_identity") return identity;
      if (cmd === "get_settings") return { hosts: [keysConn] };
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("radio", { name: "Access keys" }));
    const id = screen.getByLabelText("Access key ID");
    const secret = screen.getByLabelText("Secret access key");
    const token = screen.getByLabelText(/Session token/);
    for (const el of [id, secret, token]) {
      expect(el).toHaveAttribute("type", "password");
      expect(el).toHaveAttribute("autocomplete", "off");
    }
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "K" } });
    fireEvent.change(id, { target: { value: "AKIA" } });
    fireEvent.change(secret, { target: { value: "shh" } });
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    await screen.findByText(`Signed in as ${identity.arn}`);
    expect(invoke).toHaveBeenCalledWith(
      "add_aws_keys_connection",
      expect.objectContaining({ accessKeyId: "AKIA", secretAccessKey: "shh", sessionToken: null }),
    );
    expect(screen.getByLabelText("Access key ID")).toHaveValue("");
    expect(screen.getByLabelText("Secret access key")).toHaveValue("");
  });

  it("check shows Insufficient with the denied actions and every policy file to copy", async () => {
    const adminFiles = [
      { name: "environment-admin.json", json: '{"Statement":"admin"}' },
      { name: "environment-admin-network.json", json: '{"Statement":"network"}' },
    ];
    invoke.mockImplementation(async (cmd: string, args?: { tier: string }) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions") return report({});
      if (cmd === "aws_policy_document")
        return args?.tier === "environment_admin"
          ? adminFiles
          : [{ name: "workspace-user.json", json: '{"Version":"2012-10-17"}' }];
      throw new Error(`unexpected ${cmd}`);
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    expect(await screen.findByText("Insufficient")).toBeInTheDocument();
    expect(screen.getByText("ecs:RunTask")).toBeInTheDocument();
    expect(screen.getByText(/SCPs or session policies/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show workspace-user policy" }));
    fireEvent.click(await screen.findByRole("button", { name: "Copy workspace-user.json" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('{"Version":"2012-10-17"}'));
    expect(invoke).toHaveBeenCalledWith("aws_policy_document", { tier: "workspace_user" });
    expect(screen.getByText("workspace-user.json copied to clipboard.")).toBeInTheDocument();

    // The admin tier is every one of its files, each shown and copied on its own.
    fireEvent.click(screen.getByRole("button", { name: "Show admin policies" }));
    expect(await screen.findByText(/Attach all 2 files/)).toBeInTheDocument();
    for (const f of adminFiles) {
      expect(screen.getByText(f.json)).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: `Copy ${f.name}` }));
      await waitFor(() => expect(writeText).toHaveBeenCalledWith(f.json));
    }
    expect(invoke).toHaveBeenCalledWith("aws_policy_document", { tier: "environment_admin" });
  });

  it("shows a neutral Incomplete badge when the only blockers are failed checks", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions")
        return report({
          tiers: [
            { tier: "workspace_user", results: [{ action: "ecs:RunTask", verdict: "unverified", note: "throttled" }] },
            { tier: "environment_admin", results: [] },
          ],
        });
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    const badge = await screen.findByText("Incomplete — check again");
    expect(badge.className).not.toContain("--error");
    expect(screen.queryByText("Insufficient")).not.toBeInTheDocument();
    expect(screen.getByText(/Some checks couldn't complete/)).toBeInTheDocument();
  });

  it("explains denied own-principal checks as the first thing to ask an admin for", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions")
        return report({
          tiers: [
            { tier: "workspace_user", results: [{ action: "iam:SimulatePrincipalPolicy", verdict: "denied", note: null }] },
            { tier: "environment_admin", results: [] },
          ],
        });
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    expect(await screen.findByText(/couldn't inspect the caller's own permissions/)).toBeInTheDocument();
  });

  it("shows throttling notes with a check-again hint", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions")
        return report({
          tiers: [
            { tier: "workspace_user", results: [{ action: "ecs:RunTask", verdict: "unverified", note: "Throttling" }] },
            { tier: "environment_admin", results: [] },
          ],
        });
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    expect(await screen.findByText(/Throttling/)).toBeInTheDocument();
    expect(screen.getAllByText(/Check again/).length).toBeGreaterThan(0);
  });

  it("a probed check says writes are unverified", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions")
        return report({
          method: { probed: { reason: "iam:SimulatePrincipalPolicy denied" } },
          summary: "workspace_user",
        });
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    expect(await screen.findByText("Workspace user (unverified)")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Simulation not permitted (iam:SimulatePrincipalPolicy denied). Read access was probed; write actions are unverified and will be confirmed on first use.",
      ),
    ).toBeInTheDocument();
  });

  it("asks for confirmation before removing", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "remove_host_connection") return { hosts: [keysConn] };
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Remove Prod" }));
    expect(invoke).not.toHaveBeenCalledWith("remove_host_connection", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "Confirm remove Prod" }));
    await waitFor(() => expect(screen.queryByText("Prod")).toBeNull());
    expect(invoke).toHaveBeenCalledWith("remove_host_connection", { id: "h1" });
  });

  it("never renders a no-op Sign in when the add itself rejects with sso_expired", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return ["dev"];
      if (cmd === "add_aws_profile_connection") throw { kind: "sso_expired", profile: "dev" };
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    await screen.findByRole("option", { name: "dev" });
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Dev" } });
    fireEvent.change(screen.getByLabelText("Profile"), { target: { value: "dev" } });
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    expect(await screen.findByText("Your SSO session for profile dev has expired.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sign in" })).toBeNull();
    expect(screen.getByText("aws sso login --profile dev")).toBeInTheDocument();
  });

  it("never re-runs the add when the settings refresh fails after a successful add", async () => {
    let settingsCalls = 0;
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return ["dev"];
      if (cmd === "add_aws_profile_connection") return conn;
      if (cmd === "get_settings") {
        if (settingsCalls++ === 0) throw "boom";
        return { hosts: [conn] };
      }
      if (cmd === "aws_connection_identity") return identity;
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    await screen.findByRole("option", { name: "dev" });
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Dev" } });
    fireEvent.change(screen.getByLabelText("Profile"), { target: { value: "dev" } });
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    expect(await screen.findByText(/could not be refreshed/)).toBeInTheDocument();
    expect(invoke.mock.calls.filter((c) => c[0] === "add_aws_profile_connection")).toHaveLength(1);
  });

  it("shows a message when the profile list cannot be read", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") throw "no config";
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    expect(await screen.findByText(/Couldn't read your AWS profiles: no config/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Access keys" }));
    expect(screen.getByLabelText("Access key ID")).toBeInTheDocument();
  });

  it("shows a message when copying the policy fails", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions") return report({});
      if (cmd === "aws_policy_document") return [{ name: "environment-admin.json", json: "{}" }];
      throw new Error(`unexpected ${cmd}`);
    });
    Object.assign(navigator, { clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) } });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    fireEvent.click(await screen.findByRole("button", { name: "Show admin policies" }));
    fireEvent.click(await screen.findByRole("button", { name: "Copy environment-admin.json" }));
    expect(await screen.findByText(/Could not copy environment-admin.json: denied/)).toBeInTheDocument();
  });

  it("shows a message when the policy files can't be loaded", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions") return report({});
      throw new Error("boom");
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    fireEvent.click(await screen.findByRole("button", { name: "Show admin policies" }));
    expect(await screen.findByText(/Could not load the policy: boom/)).toBeInTheDocument();
  });

  it.each([
    [{ kind: "profile_not_found", profile: "ghost" }, "Profile ghost was not found in your AWS config, or has no credentials."],
    [
      { kind: "keys_missing" },
      "The access keys for this connection are missing from the keychain. Remove and re-add the connection.",
    ],
  ])("a failed Check shows its error: %j", async (err, text) => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions") throw err;
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(screen.queryByTestId("permission-report")).toBeNull();
  });

  it("moves focus to Confirm when removing, and back to Remove on cancel", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      throw new Error(`unexpected ${cmd}`);
    });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Remove Prod" }));
    expect(screen.getByRole("button", { name: "Confirm remove Prod" })).toHaveFocus();
    fireEvent.click(within(screen.getByTestId("host-h1")).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Remove Prod" })).toHaveFocus());
  });
});
