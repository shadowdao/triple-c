import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import ImportSettingsModal from "./ImportSettingsModal";
import type { AppSettings, SettingsImportPreview } from "../../lib/types";

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
};

describe("ImportSettingsModal", () => {
  it("keeps 'Choose file' disabled until a password is entered", () => {
    render(<ImportSettingsModal onClose={vi.fn()} onImported={vi.fn()} />);
    expect(screen.getByRole("button", { name: /choose file/i })).toBeDisabled();

    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "hunter2" } });
    expect(screen.getByRole("button", { name: /choose file/i })).not.toBeDisabled();
  });

  it("shows the preview and confirms with the same password used to open it", async () => {
    previewSettingsImport.mockResolvedValue(samplePreview);
    applySettingsImport.mockResolvedValue({} as AppSettings);
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
