import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ConfirmRemoveModal, { ConfirmForgetAwsModal } from "./ConfirmRemoveModal";

describe("ConfirmRemoveModal", () => {
  it("keeps an AWS workspace's access point unless asked", () => {
    const onConfirm = vi.fn();
    render(<ConfirmRemoveModal projectName="api" aws savedFiles onConfirm={onConfirm} onCancel={vi.fn()} />);
    const box = screen.getByRole("checkbox", { name: /file-system access point/ });
    expect(box).not.toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    fireEvent.click(box);
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(onConfirm.mock.calls).toEqual([[{ discardData: false }], [{ discardData: true }]]);
  });

  it("says both access points go when the workspace kept /workspace on EFS", () => {
    render(
      <ConfirmRemoveModal projectName="api" aws savedFiles workspaceFiles onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(screen.getByRole("checkbox", { name: /file-system access points/ })).toBeInTheDocument();
    expect(screen.getByText(/\/workspace/)).toBeInTheDocument();
  });

  it("offers no access-point choice for an Ephemeral AWS workspace", () => {
    const onConfirm = vi.fn();
    render(<ConfirmRemoveModal projectName="api" aws onConfirm={onConfirm} onCancel={vi.fn()} />);
    expect(screen.queryByRole("checkbox")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(onConfirm).toHaveBeenCalledWith({ discardData: false });
  });

  it("offers no access-point choice for a local project", () => {
    render(<ConfirmRemoveModal projectName="api" onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.getByText(/mounted in, not copied/)).toBeInTheDocument();
  });

  it("says what is left in AWS when the connection is gone", () => {
    const onConfirm = vi.fn();
    render(
      <ConfirmForgetAwsModal
        projectName="api"
        reason="This project's AWS connection no longer exists"
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText(/task definitions, its secret and its saved files/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove and leave them in AWS" }));
    expect(onConfirm).toHaveBeenCalled();
  });
});
