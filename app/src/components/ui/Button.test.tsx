import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import Button from "./Button";

const onClick = vi.fn();
const onKeyDown = vi.fn();

describe("Button", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("still supports the native disabled attribute", () => {
    render(
      <Button disabled onClick={onClick}>
        Save
      </Button>,
    );
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("stays in the accessibility tree when unavailable, and says why", () => {
    render(
      <Button unavailable unavailableReason="Stop the container first.">
        Save
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Save" });
    expect(button).not.toBeDisabled();
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAccessibleDescription("Stop the container first.");
    // The reason is a description, not part of the name.
    expect(button).toHaveAccessibleName("Save");
  });

  it("guards clicks and Enter/Space while unavailable", () => {
    render(
      <Button unavailable unavailableReason="Stop the container first." onClick={onClick}>
        Save
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Save" });
    fireEvent.click(button);
    fireEvent.keyDown(button, { key: "Enter" });
    fireEvent.keyDown(button, { key: " " });
    expect(onClick).not.toHaveBeenCalled();
  });

  it("still forwards keys that are not activation keys", () => {
    render(
      <Button
        unavailable
        unavailableReason="Stop the container first."
        onKeyDown={onKeyDown}
      >
        Save
      </Button>,
    );
    fireEvent.keyDown(screen.getByRole("button", { name: "Save" }), {
      key: "Escape",
    });
    expect(onKeyDown).toHaveBeenCalled();
  });

  it("behaves like an ordinary button when available", () => {
    render(
      <Button unavailable={false} unavailableReason="Stop the container first." onClick={onClick}>
        Save
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Save" });
    expect(button).not.toHaveAttribute("aria-disabled");
    expect(button).toHaveAccessibleDescription("");
    fireEvent.click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });
});
