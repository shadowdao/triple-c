import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen } from "@testing-library/react";
import type { AwsWorkspaceConfig, HourlyEstimate } from "../../../lib/types";
import { defaultAwsWorkspace } from "../../../lib/awsWorkspace";

const awsCostEstimate =
  vi.fn<
    (id: string, region: string, config: AwsWorkspaceConfig, instanceType: string | null) => Promise<HourlyEstimate | null>
  >();
vi.mock("../../../lib/tauri-commands", () => ({
  awsCostEstimate: (id: string, region: string, config: AwsWorkspaceConfig, instanceType: string | null) =>
    awsCostEstimate(id, region, config, instanceType),
}));

import AwsCostEstimate, { ESTIMATE_DEBOUNCE_MS, formatHourly } from "./AwsCostEstimate";

const config = (over: Partial<AwsWorkspaceConfig> = {}): AwsWorkspaceConfig => ({
  ...defaultAwsWorkspace(),
  storage: "ephemeral",
  ...over,
});

async function settle() {
  await act(async () => {
    vi.advanceTimersByTime(ESTIMATE_DEBOUNCE_MS);
  });
  await act(async () => {});
}

beforeEach(() => {
  vi.useFakeTimers();
  awsCostEstimate.mockReset();
});
afterEach(() => vi.useRealTimers());

describe("AwsCostEstimate", () => {
  it("formats small and larger figures", () => {
    expect(formatHourly(0.05826)).toBe("$0.058");
    expect(formatHourly(0.2468)).toBe("$0.25");
    expect(formatHourly(2.5)).toBe("$2.50");
  });

  it("shows the on-demand hourly figure for the connection, region and config", async () => {
    awsCostEstimate.mockResolvedValue({
      usd_per_hour: 0.05826,
      note_spot: false,
      efs_billed_separately: false,
      ec2_instance: false,
    });
    const c = config();
    render(<AwsCostEstimate connectionId="c1" region="us-west-2" config={c} />);
    expect(screen.getByText("Estimating…")).toBeInTheDocument();
    await settle();
    expect(awsCostEstimate).toHaveBeenCalledWith("c1", "us-west-2", c, null);
    expect(screen.getByText("≈ $0.058/hour")).toBeInTheDocument();
    expect(screen.queryByText(/Spot/)).toBeNull();
    expect(screen.queryByText(/EFS/)).toBeNull();
  });

  it("notes Spot, EFS and the gateway sidecar", async () => {
    awsCostEstimate.mockResolvedValue({
      usd_per_hour: 0.05,
      note_spot: true,
      efs_billed_separately: true,
      ec2_instance: false,
    });
    render(
      <AwsCostEstimate
        connectionId="c1"
        region="us-west-2"
        config={config({ spot: true, storage: "hybrid", gateway_sidecar: true })}
      />,
    );
    await settle();
    expect(
      screen.getByText("≈ $0.050/hour (on-demand; Spot is usually up to 70% cheaper)"),
    ).toBeInTheDocument();
    expect(screen.getByText(/EFS storage is billed separately/)).toBeInTheDocument();
    expect(screen.getByText(/gateway sidecar shares the task's CPU and memory/)).toBeInTheDocument();
  });

  it("says the estimate is unavailable for null or a failed call, never an error", async () => {
    awsCostEstimate.mockResolvedValueOnce(null);
    const { unmount } = render(<AwsCostEstimate connectionId="c1" region="us-west-2" config={config()} />);
    await settle();
    expect(screen.getByText("Estimate unavailable")).toBeInTheDocument();
    unmount();

    awsCostEstimate.mockRejectedValueOnce("boom");
    render(<AwsCostEstimate connectionId="c1" region="us-west-2" config={config()} />);
    await settle();
    expect(screen.getByText("Estimate unavailable")).toBeInTheDocument();
  });

  it("without a connection or region it does not ask", async () => {
    render(<AwsCostEstimate connectionId={null} region="us-west-2" config={config()} />);
    await settle();
    expect(awsCostEstimate).not.toHaveBeenCalled();
    expect(screen.getByText("Estimate unavailable")).toBeInTheDocument();
  });

  it("asks once per settled change of a cost field, not for unrelated edits", async () => {
    awsCostEstimate.mockResolvedValue({ usd_per_hour: 0.1, note_spot: false, efs_billed_separately: false, ec2_instance: false });
    const { rerender } = render(<AwsCostEstimate connectionId="c1" region="us-west-2" config={config()} />);
    await settle();
    expect(awsCostEstimate).toHaveBeenCalledTimes(1);
    // An unrelated field: no new call.
    rerender(<AwsCostEstimate connectionId="c1" region="us-west-2" config={config({ setup_script: "x" })} />);
    await settle();
    expect(awsCostEstimate).toHaveBeenCalledTimes(1);
    // Two quick CPU changes: one call, with the last.
    rerender(<AwsCostEstimate connectionId="c1" region="us-west-2" config={config({ cpu: 2048 })} />);
    rerender(
      <AwsCostEstimate connectionId="c1" region="us-west-2" config={config({ cpu: 4096, memory_mib: 8192 })} />,
    );
    await settle();
    expect(awsCostEstimate).toHaveBeenCalledTimes(2);
    expect(awsCostEstimate.mock.calls[1][2].cpu).toBe(4096);
  });

  it("prices an EC2 project as its instance type, and says instances scale to zero", async () => {
    awsCostEstimate.mockResolvedValue({ usd_per_hour: 0.1008, note_spot: false, efs_billed_separately: true, ec2_instance: true });
    const c = config({ launch_type: "ec2", storage: "hybrid" });
    const { rerender } = render(
      <AwsCostEstimate connectionId="c1" region="us-west-2" config={c} instanceType="m7i.large" />,
    );
    await settle();
    expect(awsCostEstimate).toHaveBeenCalledWith("c1", "us-west-2", c, "m7i.large");
    expect(screen.getByText("≈ $0.10/hour")).toBeInTheDocument();
    expect(screen.getByText(/Instances scale to zero when no workspace is running/)).toBeInTheDocument();
    expect(screen.getByText(/EFS storage is billed separately/)).toBeInTheDocument();
    expect(screen.queryByText(/While the task runs/)).toBeNull();
    // The reservations don't change an instance's price: no new call.
    rerender(
      <AwsCostEstimate connectionId="c1" region="us-west-2" config={{ ...c, cpu: 512, memory_mib: 1024 }} instanceType="m7i.large" />,
    );
    await settle();
    expect(awsCostEstimate).toHaveBeenCalledTimes(1);
  });

  it("does not ask for an EC2 project whose instance type is unknown", async () => {
    render(<AwsCostEstimate connectionId="c1" region="us-west-2" config={config({ launch_type: "ec2" })} instanceType={null} />);
    await settle();
    expect(awsCostEstimate).not.toHaveBeenCalled();
    expect(screen.getByText("Estimate unavailable")).toBeInTheDocument();
  });
});
