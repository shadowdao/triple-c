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

  it("check shows Insufficient with the denied actions and the copy-policy buttons", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_aws_profiles") return [];
      if (cmd === "check_aws_permissions") return report({});
      if (cmd === "aws_policy_document") return '{"Version":"2012-10-17"}';
      throw new Error(`unexpected ${cmd}`);
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<HostsSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Check Prod" }));
    expect(await screen.findByText("Insufficient")).toBeInTheDocument();
    expect(screen.getByText("ecs:RunTask")).toBeInTheDocument();
    expect(screen.getByText(/SCPs, permission boundaries/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy workspace-user policy" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('{"Version":"2012-10-17"}'));
    expect(invoke).toHaveBeenCalledWith("aws_policy_document", { tier: "workspace_user" });
    expect(screen.getByRole("button", { name: "Copy admin policy" })).toBeInTheDocument();
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
    expect(screen.getByText(/Check again/)).toBeInTheDocument();
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
});
