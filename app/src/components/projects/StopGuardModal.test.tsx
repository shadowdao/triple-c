import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import StopGuardModal from "./StopGuardModal";
import type { UnsavedWork } from "../../lib/types";

const dirty: UnsavedWork = {
  checked: true,
  repos: [
    { folder: "api", branch: "main", dirty: true, unpushed: 2 },
    { folder: "clean", branch: "main", dirty: false, unpushed: 0 },
  ],
  error: null,
};

describe("StopGuardModal", () => {
  it("lists the repositories at risk and defaults to pushing", async () => {
    const onChoose = vi.fn();
    render(<StopGuardModal projectName="api" work={dirty} onChoose={onChoose} />);
    expect(screen.getByText("/workspace/api")).toBeInTheDocument();
    expect(screen.getByText(/uncommitted changes, 2 unpushed commits/)).toBeInTheDocument();
    expect(screen.queryByText("/workspace/clean")).toBeNull();
    const push = screen.getByRole("button", { name: "Push to WIP branch and stop" });
    await waitFor(() => expect(push).toHaveFocus());
    fireEvent.click(push);
    expect(onChoose).toHaveBeenCalledWith("push");
  });

  it("lists a repository still cloning as having nothing to push", () => {
    const work: UnsavedWork = {
      ...dirty,
      repos: [...dirty.repos, { folder: "big", branch: "", dirty: false, unpushed: 0, cloning: true }],
    };
    render(<StopGuardModal projectName="api" work={work} onChoose={vi.fn()} />);
    expect(screen.getByText("/workspace/big")).toBeInTheDocument();
    expect(screen.getByText("still cloning — nothing to push")).toBeInTheDocument();
    expect(screen.getByText("/workspace/api")).toBeInTheDocument();
  });

  it("offers Stop without pushing and Cancel", () => {
    const onChoose = vi.fn();
    render(<StopGuardModal projectName="api" work={dirty} onChoose={onChoose} />);
    fireEvent.click(screen.getByRole("button", { name: "Stop without pushing" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onChoose.mock.calls).toEqual([["discard"], [null]]);
  });

  it("hides Push and says why when the workspace could not be checked", async () => {
    const onChoose = vi.fn();
    render(
      <StopGuardModal
        projectName="api"
        work={{ checked: false, repos: [], error: "Triple-C is not connected to the workspace" }}
        wipPushOnStop
        onChoose={onChoose}
      />,
    );
    expect(screen.queryByRole("button", { name: /Push/ })).toBeNull();
    expect(screen.getByText(/own push of work in progress still runs/)).toBeInTheDocument();
    expect(screen.getByText("Triple-C is not connected to the workspace")).toBeInTheDocument();
    expect(screen.getByText(/could not check/)).toBeInTheDocument();
    const stop = screen.getByRole("button", { name: "Stop anyway" });
    await waitFor(() => expect(stop).toHaveFocus());
    // Nothing is recorded: the shutdown push the copy promises still runs.
    fireEvent.click(stop);
    expect(onChoose).toHaveBeenCalledWith("unguarded");
  });
});
