import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import SegmentedControl from "./SegmentedControl";

describe("SegmentedControl", () => {
  it("a disabled segment can't be clicked and arrow keys skip it", () => {
    const onChange = vi.fn();
    render(
      <SegmentedControl
        label="Pick"
        value="a"
        onChange={onChange}
        segments={[
          { value: "a", label: "A" },
          { value: "b", label: "B", disabled: true },
          { value: "c", label: "C" },
        ]}
      />,
    );
    expect(screen.getByRole("radio", { name: "B" })).toBeDisabled();
    fireEvent.click(screen.getByRole("radio", { name: "B" }));
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByRole("radiogroup"), { key: "ArrowRight" });
    expect(onChange).toHaveBeenLastCalledWith("c");
    fireEvent.keyDown(screen.getByRole("radiogroup"), { key: "ArrowLeft" });
    expect(onChange).toHaveBeenLastCalledWith("c");
  });

  it("with every other segment disabled, the arrows stay put", () => {
    const onChange = vi.fn();
    render(
      <SegmentedControl
        label="Pick"
        value="a"
        onChange={onChange}
        segments={[
          { value: "a", label: "A" },
          { value: "b", label: "B", disabled: true },
        ]}
      />,
    );
    fireEvent.keyDown(screen.getByRole("radiogroup"), { key: "ArrowRight" });
    expect(onChange).not.toHaveBeenCalled();
  });
});
