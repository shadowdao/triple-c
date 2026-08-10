import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import CaCertPathInput from "./CaCertPathInput";
import type { CaCertInfo } from "../../lib/types";

const inspectCaCertPath = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  inspectCaCertPath: (path: string) => inspectCaCertPath(path),
}));

const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (opts: unknown) => openDialog(opts),
}));

const info = (over: Partial<CaCertInfo> = {}): CaCertInfo => ({
  exists: true,
  is_directory: false,
  cert_count: 1,
  installed_names: ["corp-root.crt"],
  error: null,
  ...over,
});

function renderInput(value = "", over: Partial<Parameters<typeof CaCertPathInput>[0]> = {}) {
  const onChange = vi.fn();
  const onCommit = vi.fn();
  const utils = render(
    <CaCertPathInput
      value={value}
      onChange={onChange}
      onCommit={onCommit}
      inputClassName="input"
      {...over}
    />,
  );
  return { onChange, onCommit, ...utils };
}

describe("CaCertPathInput", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    inspectCaCertPath.mockResolvedValue(info());
  });

  it("does not inspect anything while the path is empty", async () => {
    renderInput("");
    await new Promise((r) => setTimeout(r, 350));
    expect(inspectCaCertPath).not.toHaveBeenCalled();
  });

  it("shows the empty hint instead of a status when unset", () => {
    renderInput("", { emptyHint: "Using the global certificate." });
    expect(screen.getByText("Using the global certificate.")).toBeTruthy();
  });

  it("reports the certificate count and the names they are installed as", async () => {
    // The rename is the whole point: update-ca-certificates ignores a .pem.
    inspectCaCertPath.mockResolvedValue(
      info({ cert_count: 2, installed_names: ["corp-root.crt", "corp-intermediate.crt"] }),
    );
    renderInput("/certs");
    await waitFor(() => expect(screen.getByText(/Found 2 certificates/)).toBeTruthy());
    expect(screen.getByText(/corp-root\.crt, corp-intermediate\.crt/)).toBeTruthy();
  });

  it("uses the singular for one certificate", async () => {
    renderInput("/certs/corp.pem");
    await waitFor(() => expect(screen.getByText(/Found 1 certificate$|Found 1 certificate/)).toBeTruthy());
    expect(screen.queryByText(/Found 1 certificates/)).toBeNull();
  });

  it("surfaces an unusable path inline rather than silently accepting it", async () => {
    inspectCaCertPath.mockResolvedValue(
      info({ exists: false, cert_count: 0, installed_names: [], error: "path does not exist" }),
    );
    renderInput("/gone");
    await waitFor(() => expect(screen.getByText(/path does not exist/)).toBeTruthy());
  });

  it("commits on blur", () => {
    const { onCommit } = renderInput("/certs");
    fireEvent.blur(screen.getByRole("textbox"));
    expect(onCommit).toHaveBeenCalledWith("/certs");
  });

  it("offers both a file and a folder picker, because the setting accepts either", async () => {
    openDialog.mockResolvedValue("/picked/corp.pem");
    const { onChange, onCommit } = renderInput("");

    fireEvent.click(screen.getByText("File…"));
    await waitFor(() => expect(onCommit).toHaveBeenCalledWith("/picked/corp.pem"));
    expect(openDialog).toHaveBeenCalledWith({ directory: false, multiple: false });

    openDialog.mockResolvedValue("/picked/certs");
    fireEvent.click(screen.getByText("Folder…"));
    await waitFor(() => expect(openDialog).toHaveBeenLastCalledWith({ directory: true, multiple: false }));
    expect(onChange).toHaveBeenCalledWith("/picked/certs");
  });

  it("does not commit when the picker is dismissed", async () => {
    openDialog.mockResolvedValue(null);
    const { onCommit } = renderInput("");
    fireEvent.click(screen.getByText("Folder…"));
    await new Promise((r) => setTimeout(r, 0));
    expect(onCommit).not.toHaveBeenCalled();
  });

  it("disables the inputs when the container is running", () => {
    renderInput("/certs", { disabled: true });
    expect((screen.getByRole("textbox") as HTMLInputElement).disabled).toBe(true);
    for (const label of ["File…", "Folder…"]) {
      expect((screen.getByText(label) as HTMLButtonElement).disabled).toBe(true);
    }
  });
});
