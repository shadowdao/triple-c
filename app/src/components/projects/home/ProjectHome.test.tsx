import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import ProjectHome from "./ProjectHome";
import { useAppState } from "../../../store/appState";
import { awsProject } from "../../../test/awsFixtures";
import { AWS_CONNECTION_MISSING } from "../../../lib/types";

const remove = vi.fn();
const project = awsProject({ name: "api" });

vi.mock("../../../hooks/useProjects", () => ({
  useProjects: () => ({ projects: [project], remove }),
}));
vi.mock("../../../hooks/useProjectActions", () => ({
  useProjectActions: () => ({
    busy: false,
    backingUp: false,
    handleStart: vi.fn(),
    handleStop: vi.fn(),
    handleReset: vi.fn(),
    handleBackup: vi.fn(),
    openClaudeTerminal: vi.fn(),
    openShell: vi.fn(),
    stopGuard: null,
    resolveStopGuard: vi.fn(),
  }),
}));
vi.mock("../../../hooks/useSaveState", () => ({
  useProjectSave: () => ({ save: vi.fn(), saveState: { status: "idle", error: null } }),
}));
vi.mock("../../../hooks/useContainerMigration", () => ({
  useContainerMigration: () => ({ running: false, interrupted: false, probeSettled: true, staleness: null }),
}));
vi.mock("./OverviewTab", () => ({ default: () => <div>overview</div> }));

const clean = { container: null, image: null, volumes: [], retry_scheduled: false };

beforeEach(() => {
  vi.clearAllMocks();
  useAppState.setState({ toasts: [] } as never);
});

async function confirmRemove(discard = false) {
  render(<ProjectHome projectId={project.id} active />);
  fireEvent.click(screen.getByRole("button", { name: "More actions" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Remove project…" }));
  const dialog = screen.getByRole("dialog");
  if (discard) fireEvent.click(within(dialog).getByRole("checkbox"));
  fireEvent.click(within(dialog).getByRole("button", { name: "Remove" }));
}

describe("ProjectHome removal of an AWS project", () => {
  it("passes the saved-files choice", async () => {
    remove.mockResolvedValue(clean);
    await confirmRemove(true);
    await waitFor(() => expect(remove).toHaveBeenCalledWith(project.id, { discardData: true }));
  });

  it("asks again when the connection is gone, then leaves the resources in AWS", async () => {
    remove
      .mockRejectedValueOnce(`${AWS_CONNECTION_MISSING}This project's AWS connection no longer exists, so …`)
      .mockResolvedValueOnce(clean);
    await confirmRemove();
    const second = await screen.findByRole("dialog", { name: "Remove without its AWS resources?" });
    // The marker is not shown to the user.
    expect(within(second).getByText(/connection no longer exists/).textContent).not.toContain(AWS_CONNECTION_MISSING);
    fireEvent.click(within(second).getByRole("button", { name: "Remove and leave them in AWS" }));
    await waitFor(() =>
      expect(remove).toHaveBeenLastCalledWith(project.id, { discardData: false, forgetAwsResources: true }),
    );
  });

  it("toasts any other failure without a second confirm", async () => {
    remove.mockRejectedValueOnce("Could not stop the task: ExpiredToken");
    await confirmRemove();
    await waitFor(() => expect(remove).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("dialog")).toBeNull();
    const toasts = () => (useAppState.getState() as unknown as { toasts: { detail?: string }[] }).toasts;
    await waitFor(() => expect(toasts().at(-1)?.detail).toMatch(/ExpiredToken/));
  });
});
