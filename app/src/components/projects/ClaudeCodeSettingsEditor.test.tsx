import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ClaudeCodeSettingsEditor, { CLAUDE_CODE_DEFAULTS } from "./ClaudeCodeSettingsEditor";
import type { ClaudeCodeSettings } from "../../lib/types";

function renderEditor(
  settings: ClaudeCodeSettings | null,
  scope: "global" | "project" = "global",
) {
  const onSave = vi.fn().mockResolvedValue(undefined);
  render(
    <ClaudeCodeSettingsEditor
      scope={scope}
      settings={settings}
      disabled={false}
      onSave={onSave}
    />,
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

  describe("project scope", () => {
    it("offers Global as a third state so a project can decline to have an opinion", () => {
      renderEditor(null, "project");
      const focus = screen.getByLabelText("Focus mode");
      expect(
        Array.from(focus.querySelectorAll("option")).map((o) => o.getAttribute("value")),
      ).toEqual(["global", "off", "on"]);
      expect((focus as HTMLSelectElement).value).toBe("global");
    });

    it("stores a deliberate false so the project can turn a global On back off", () => {
      // The reason the field widened from boolean to boolean|null. Under the
      // old merge there was no project value that could produce this.
      const onSave = renderEditor(null, "project");
      fireEvent.change(screen.getByLabelText("Focus mode"), { target: { value: "off" } });
      expect(onSave).toHaveBeenCalledWith(
        expect.objectContaining({ focus_mode: false }),
      );
    });

    it("does not collapse a deliberate off to null", () => {
      // `null` means inherit. Collapsing here would silently hand the setting
      // straight back to the global value the user just overrode.
      const onSave = renderEditor(null, "project");
      fireEvent.change(screen.getByLabelText("Focus mode"), { target: { value: "off" } });
      expect(onSave).not.toHaveBeenCalledWith(null);
    });

    it("round-trips the inverted fields through the disabled sense", () => {
      // Session recap stores `session_recap_disabled`, so choosing "off" has to
      // store `true` and choosing "on" has to store `false`.
      const onSave = renderEditor(null, "project");
      const recap = screen.getByLabelText("Session recap");

      fireEvent.change(recap, { target: { value: "off" } });
      expect(onSave).toHaveBeenCalledWith(
        expect.objectContaining({ session_recap_disabled: true }),
      );

      fireEvent.change(recap, { target: { value: "on" } });
      expect(onSave).toHaveBeenCalledWith(
        expect.objectContaining({ session_recap_disabled: false }),
      );
    });

    it("shows a stored override rather than the inherited state", () => {
      renderEditor({ ...CLAUDE_CODE_DEFAULTS, session_recap_disabled: true }, "project");
      expect((screen.getByLabelText("Session recap") as HTMLSelectElement).value).toBe(
        "off",
      );
    });

    /**
     * Auto-scroll is the second inverted field and had no project-scope test at
     * all — every assertion above rides on `session_recap_disabled`, so a
     * `BOOLEAN_FIELDS` entry that lost its `invert` flag would be caught for
     * one of the two and pass silently for the other. It is stored as
     * `auto_scroll_disabled`, so every value here reads back the other way up.
     */
    describe("auto-scroll", () => {
      const AUTO = "Auto-scroll";

      it("starts on Global, which is not the same as on", () => {
        // Claude Code scrolls by default, so an inheriting project *behaves*
        // as on — but it has taken no position, and rendering it as "On" would
        // make a later global change look like it had no effect.
        renderEditor(null, "project");
        expect((screen.getByLabelText(AUTO) as HTMLSelectElement).value).toBe("global");
      });

      it("stores the disabled sense in both directions", () => {
        const onSave = renderEditor(null, "project");
        const auto = screen.getByLabelText(AUTO);

        fireEvent.change(auto, { target: { value: "off" } });
        expect(onSave).toHaveBeenCalledWith(
          expect.objectContaining({ auto_scroll_disabled: true }),
        );

        fireEvent.change(auto, { target: { value: "on" } });
        expect(onSave).toHaveBeenCalledWith(
          expect.objectContaining({ auto_scroll_disabled: false }),
        );
      });

      it("hands the setting back to the global level when Global is chosen", () => {
        // Back to no opinion, and with nothing else set that collapses the
        // whole object to `null` — the value that means "adds nothing over the
        // global settings".
        const onSave = renderEditor(
          { ...CLAUDE_CODE_DEFAULTS, auto_scroll_disabled: true },
          "project",
        );
        fireEvent.change(screen.getByLabelText(AUTO), { target: { value: "global" } });
        expect(onSave).toHaveBeenCalledWith(null);
      });

      it("reads a stored override back the right way up", () => {
        renderEditor({ ...CLAUDE_CODE_DEFAULTS, auto_scroll_disabled: true }, "project");
        expect((screen.getByLabelText(AUTO) as HTMLSelectElement).value).toBe("off");
      });
    });
  });

  /**
   * The inverted fields store a *deviation*, so a stored `false` is the one
   * value that means "the user deliberately re-enabled the default". Nothing
   * asserted it: every existing test drives the `true` (turned off) direction
   * or the `null` (untouched) one, and both scopes would still read correctly
   * if the inversion were dropped from the `false` branch alone.
   */
  describe.each([
    ["session_recap_disabled", "Session recap"] as const,
    ["auto_scroll_disabled", "Auto-scroll"] as const,
  ])("a stored false on %s", (key, label) => {
    it("reads as On at project scope, not as Off", () => {
      renderEditor({ ...CLAUDE_CODE_DEFAULTS, [key]: false }, "project");
      expect((screen.getByLabelText(label) as HTMLSelectElement).value).toBe("on");
    });

    it("reads as on at global scope, where the control is a switch", () => {
      renderEditor({ ...CLAUDE_CODE_DEFAULTS, [key]: false });
      expect(screen.getByRole("switch", { name: label })).toBeChecked();
    });
  });
});
