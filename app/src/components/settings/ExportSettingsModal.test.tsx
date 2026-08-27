import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import ExportSettingsModal from "./ExportSettingsModal";

const exportSettings = vi.fn();

vi.mock("../../lib/tauri-commands", () => ({
  exportSettings: (password: string) => exportSettings(password),
}));

beforeEach(() => {
  vi.clearAllMocks();
});

function fillPasswords(password: string, confirm: string) {
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: password } });
  fireEvent.change(screen.getByLabelText("Confirm password"), { target: { value: confirm } });
}

describe("ExportSettingsModal", () => {
  it("keeps the submit button disabled until the passwords are long enough and match", () => {
    render(<ExportSettingsModal onClose={vi.fn()} />);
    const submit = screen.getByRole("button", { name: /choose where to save/i });
    expect(submit).toBeDisabled();

    fillPasswords("short", "short");
    expect(submit).toBeDisabled();
    expect(screen.getByText(/use at least 8 characters/i)).toBeInTheDocument();

    fillPasswords("longenoughpassword", "different");
    expect(submit).toBeDisabled();
    expect(screen.getByText(/don't match/i)).toBeInTheDocument();

    fillPasswords("longenoughpassword", "longenoughpassword");
    expect(submit).not.toBeDisabled();
  });

  it("exports with the entered password and shows success", async () => {
    exportSettings.mockResolvedValue(true);
    render(<ExportSettingsModal onClose={vi.fn()} />);

    fillPasswords("longenoughpassword", "longenoughpassword");
    fireEvent.click(screen.getByRole("button", { name: /choose where to save/i }));

    await waitFor(() => expect(exportSettings).toHaveBeenCalledWith("longenoughpassword"));
    await waitFor(() => expect(screen.getByText(/settings exported/i)).toBeInTheDocument());
  });

  it("closes quietly when the save dialog is dismissed", async () => {
    exportSettings.mockResolvedValue(false);
    const onClose = vi.fn();
    render(<ExportSettingsModal onClose={onClose} />);

    fillPasswords("longenoughpassword", "longenoughpassword");
    fireEvent.click(screen.getByRole("button", { name: /choose where to save/i }));

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(screen.queryByText(/settings exported/i)).not.toBeInTheDocument();
  });

  it("shows an error rather than closing when the export fails", async () => {
    exportSettings.mockRejectedValue("Disk is full");
    const onClose = vi.fn();
    render(<ExportSettingsModal onClose={onClose} />);

    fillPasswords("longenoughpassword", "longenoughpassword");
    fireEvent.click(screen.getByRole("button", { name: /choose where to save/i }));

    await waitFor(() => expect(screen.getByText("Disk is full")).toBeInTheDocument());
    expect(onClose).not.toHaveBeenCalled();
  });
});
