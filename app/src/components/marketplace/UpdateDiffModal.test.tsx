import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const marketplaceItemDiff = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  marketplaceItemDiff: (...a: unknown[]) => marketplaceItemDiff(...a),
}));

import UpdateDiffModal from "./UpdateDiffModal";

const A = "a".repeat(40);
const B = "b".repeat(40);
const item = { marketplace_id: "m1", kind: "hook" as const, key: "notify" };

describe("UpdateDiffModal", () => {
  beforeEach(() => vi.clearAllMocks());

  it("loads the diff from the install's pin to head and accepts", async () => {
    marketplaceItemDiff.mockResolvedValue([
      { path: "notify.sh", change: "modified", unified: "-echo old\n+echo new\n" },
      { path: "icon.png", change: "added", unified: null },
    ]);
    const onAccept = vi.fn(async () => true);
    render(<UpdateDiffModal item={item} fromCommit={A} toCommit={B} scopeLabel="All projects" onClose={vi.fn()} onAccept={onAccept} />);
    await waitFor(() => expect(marketplaceItemDiff).toHaveBeenCalledWith(item, A, B));
    expect(screen.getByText(/\+echo new/)).toBeInTheDocument();
    expect(screen.getByText("Binary file — no text diff")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await waitFor(() => expect(onAccept).toHaveBeenCalled());
  });

  it("shows a load error and keeps Update disabled", async () => {
    marketplaceItemDiff.mockRejectedValue("commit not in cache");
    render(<UpdateDiffModal item={item} fromCommit={A} toCommit={B} scopeLabel="p" onClose={vi.fn()} onAccept={vi.fn()} />);
    expect(await screen.findByText(/commit not in cache/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Update" })).toBeDisabled();
  });

  it("shows the rendered commands a hook will run after the update (F8)", async () => {
    marketplaceItemDiff.mockResolvedValue([]);
    render(
      <UpdateDiffModal
        item={item}
        fromCommit={A}
        toCommit={B}
        scopeLabel="All projects"
        hookCommands={["/home/claude/.claude/triple-c/hooks/notify/run.sh --new-flag"]}
        onClose={vi.fn()}
        onAccept={vi.fn()}
      />,
    );
    await waitFor(() => expect(marketplaceItemDiff).toHaveBeenCalled());
    expect(screen.getByText("Commands after this update")).toBeInTheDocument();
    expect(
      screen.getByText("/home/claude/.claude/triple-c/hooks/notify/run.sh --new-flag"),
    ).toBeInTheDocument();
  });
});
