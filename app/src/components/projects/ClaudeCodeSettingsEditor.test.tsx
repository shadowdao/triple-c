import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ClaudeCodeSettingsEditor, { CLAUDE_CODE_DEFAULTS } from "./ClaudeCodeSettingsEditor";
import type { ClaudeCodeSettings } from "../../lib/types";

function renderEditor(settings: ClaudeCodeSettings | null) {
  const onSave = vi.fn().mockResolvedValue(undefined);
  render(
    <ClaudeCodeSettingsEditor settings={settings} disabled={false} onSave={onSave} />,
  );
  return onSave;
}

describe("ClaudeCodeSettingsEditor", () => {
  it("shows the two default-on settings as on for a project that never touched them", () => {
    // Claude Code's session recap and fullscreen auto-scroll are both on by
    // default, and the fields behind them store the *disabled* sense. A toggle
    // rendered straight from the field would tell every existing user their
    // recap is off.
    renderEditor(null);
    expect(screen.getByRole("switch", { name: "Session recap" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Auto-scroll" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Focus mode" })).not.toBeChecked();
  });

  it("stores the disabled sense when an inverted toggle is switched off", () => {
    const onSave = renderEditor(null);
    fireEvent.click(screen.getByRole("switch", { name: "Session recap" }));
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({ session_recap_disabled: true }),
    );
  });

  it("collapses back to null once every setting is at its default again", () => {
    // `null` is what tells the backend this project adds nothing over the
    // global settings, so the round trip has to land exactly back on it.
    const onSave = renderEditor({ ...CLAUDE_CODE_DEFAULTS, session_recap_disabled: true });
    fireEvent.click(screen.getByRole("switch", { name: "Session recap" }));
    expect(onSave).toHaveBeenCalledWith(null);
  });

  it("offers the classic renderer as a choice distinct from automatic", () => {
    // Leaving `tui` unset lets Claude Code pick; pinning "default" is a
    // different, and previously unreachable, instruction.
    const onSave = renderEditor(null);
    const tui = screen.getByLabelText("TUI mode");
    expect(
      Array.from(tui.querySelectorAll("option")).map((o) => o.getAttribute("value")),
    ).toEqual(["", "default", "fullscreen"]);
    fireEvent.change(tui, { target: { value: "default" } });
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ tui_mode: "default" }));
  });

  it("offers every effort level Claude Code accepts", () => {
    renderEditor(null);
    expect(
      Array.from(
        screen.getByLabelText("Effort level").querySelectorAll("option"),
      ).map((o) => o.getAttribute("value")),
    ).toEqual(["", "low", "medium", "high", "xhigh"]);
  });
});
