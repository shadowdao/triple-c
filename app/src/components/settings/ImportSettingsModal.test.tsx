import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import ImportSettingsModal from "./ImportSettingsModal";
import type { AppSettings, SettingsImportOutcome, SettingsImportPreview } from "../../lib/types";

const previewSettingsImport = vi.fn();
const applySettingsImport = vi.fn();

vi.mock("../../lib/tauri-commands", () => ({
  previewSettingsImport: (password: string) => previewSettingsImport(password),
  applySettingsImport: (password: string) => applySettingsImport(password),
}));

beforeEach(() => {
  vi.clearAllMocks();
});

const samplePreview: SettingsImportPreview = {
  exported_at: "2026-08-27T00:00:00Z",
  app_version: "0.4.14",
  custom_env_var_count: 2,
  gateway_model_count: 0,
  has_claude_code_settings: false,
  has_claude_oauth_token: true,
  has_gateway_api_key: false,
  has_gateway_master_key: false,
  has_web_terminal_access_token: false,
  enables_web_terminal: false,
  ollama_base_url: null,
  llamacpp_base_url: null,
  openai_compatible_base_url: null,
  gateway_api_base: null,
  image_source: "registry",
  custom_image_name: null,
  marketplace_count: 0,
  global_hook_install_count: 0,
  global_plugin_install_count: 0,
  marketplace_account_token_count: 0,
  aws_connections: [],
  aws_connection_key_count: 0,
  removed_aws_connections: [],
};

function outcome(settings: AppSettings, secretRestoreWarnings: string[] = []): SettingsImportOutcome {
  return { settings, secret_restore_warnings: secretRestoreWarnings };
}

describe("ImportSettingsModal", () => {
  it("keeps 'Choose file' disabled until a password is entered", () => {
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);
    expect(screen.getByRole("button", { name: /choose file/i })).toBeDisabled();

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    expect(screen.getByRole("button", { name: /choose file/i })).not.toBeDisabled();
  });

  it("shows the preview and confirms with the same password used to open it", async () => {
    previewSettingsImport.mockResolvedValue(samplePreview);
    applySettingsImport.mockResolvedValue(outcome({} as AppSettings));
    const onImported = vi.fn();
    render(<ImportSettingsModal onClose={vi.fn()} onImported={onImported} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));

    await waitFor(() => expect(previewSettingsImport).toHaveBeenCalledWith("hunter2"));
    expect(await screen.findByText(/2 global custom env vars/i)).toBeInTheDocument();
    expect(screen.getByText(/your shared claude login/i)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    await waitFor(() => expect(applySettingsImport).toHaveBeenCalledWith("hunter2"));
    await waitFor(() => expect(onImported).toHaveBeenCalledWith({}));
    expect(await screen.findByText(/settings imported/i)).toBeInTheDocument();
  });

  it("shows a distinct warning when the import would enable the web terminal", async () => {
    previewSettingsImport.mockResolvedValue({ ...samplePreview, enables_web_terminal: true });
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));

    expect(await screen.findByText(/enables the remote web terminal/i)).toBeInTheDocument();
  });

  it("shows the AWS connections and key count, and flags overwrites", async () => {
    previewSettingsImport.mockResolvedValue({
      ...samplePreview,
      aws_connections: [
        {
          name: "Prod",
          auth: "keys",
          profile_name: null,
          default_region: "us-west-2",
          overwrites_existing: true,
          overwrites_keys: true,
          keys_check_failed: false,
          local: { name: "Real Prod", auth: "keys", profile_name: null, default_region: "us-east-1" },
        },
        {
          name: "Dev",
          auth: "profile",
          profile_name: "dev-profile",
          default_region: "eu-west-1",
          overwrites_existing: false,
          overwrites_keys: false,
          keys_check_failed: true,
          local: null,
        },
      ],
      aws_connection_key_count: 1,
      removed_aws_connections: [
        { name: "Old", auth: "profile", profile_name: "old-p", default_region: "ap-south-1" },
      ],
    });
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));

    expect(await screen.findByText(/AWS connection "Prod" \(access keys, us-west-2\)/)).toBeInTheDocument();
    expect(screen.getByText(/AWS connection "Dev" \(profile dev-profile, eu-west-1\)/)).toBeInTheDocument();
    expect(screen.getByText(/1 AWS access key set$/)).toBeInTheDocument();
    expect(screen.getByText(/Overwrites the AWS access keys stored for your connection "Real Prod"/)).toBeInTheDocument();
    expect(screen.getByText(/Replaces your connection "Real Prod" \(access keys, us-east-1\) with "Prod"/)).toBeInTheDocument();
    expect(screen.getByText(/Could not check whether keys are stored for "Dev".*may overwrite them/)).toBeInTheDocument();
    expect(screen.getByText(/Removes your AWS connection "Old" \(profile old-p, ap-south-1\)/)).toBeInTheDocument();
  });

  it("warns about a custom Docker image every time, not just on change", async () => {
    previewSettingsImport.mockResolvedValue({
      ...samplePreview,
      image_source: "custom",
      custom_image_name: "ghcr.io/attacker/triple-c:latest",
    });
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));

    expect(
      await screen.findByText(/custom docker image: ghcr\.io\/attacker\/triple-c:latest/i),
    ).toBeInTheDocument();
  });

  it("shows a secret-restore warning alongside success rather than hiding it", async () => {
    previewSettingsImport.mockResolvedValue(samplePreview);
    applySettingsImport.mockResolvedValue(
      outcome({} as AppSettings, ["Could not restore the gateway master key: keychain locked"]),
    );
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));
    await screen.findByText(/2 global custom env vars/i);

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    expect(await screen.findByText(/settings imported/i)).toBeInTheDocument();
    expect(await screen.findByText(/could not restore the gateway master key/i)).toBeInTheDocument();
  });

  it("closes quietly when the file picker is dismissed", async () => {
    previewSettingsImport.mockResolvedValue(null);
    const onClose = vi.fn();
    render(<ImportSettingsModal onClose={onClose} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));

    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("shows an error when the password is wrong rather than a blank preview", async () => {
    previewSettingsImport.mockRejectedValue("Wrong password, or the file is corrupted.");
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "wrong" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));

    expect(await screen.findByText(/wrong password, or the file is corrupted/i)).toBeInTheDocument();
  });

  it("shows an error if applying the import fails, without claiming success", async () => {
    previewSettingsImport.mockResolvedValue(samplePreview);
    applySettingsImport.mockRejectedValue("Keychain write failed");
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /choose file/i }));
    await screen.findByText(/2 global custom env vars/i);

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    expect(await screen.findByText("Keychain write failed")).toBeInTheDocument();
    expect(screen.queryByText(/settings imported/i)).not.toBeInTheDocument();
  });
});
