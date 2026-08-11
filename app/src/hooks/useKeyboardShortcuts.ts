import { useEffect } from "react";
import { useAppState, isTerminalTab, tabKeyId } from "../store/appState";
import { useTerminal } from "./useTerminal";

/**
 * Whether the focus is in something the user is editing text in.
 *
 * xterm's hidden textarea is deliberately excluded: it is an input-method
 * shim, not a field anyone edits word-wise, and the terminal is exactly where
 * the tab shortcuts need to keep working.
 */
function inTextField(el: Element | null): boolean {
  if (!el || el.closest(".xterm")) return false;
  return (
    el.tagName === "INPUT" ||
    el.tagName === "TEXTAREA" ||
    (el as HTMLElement).isContentEditable === true
  );
}

/**
 * App-level shortcuts. Registered on `document` in the *capture* phase so they
 * win over xterm.js, which would otherwise forward them to the shell inside
 * the container.
 *
 *   Ctrl+T        new Claude terminal for the current project
 *   Ctrl+Shift+W  close the active tab
 *   Ctrl+Tab      next tab (Ctrl+Shift+Tab for previous)
 *   Ctrl+1..9     jump to the nth tab
 *   Ctrl+Shift+←/→  move the active tab along the strip
 */
export function useKeyboardShortcuts() {
  const { open: openTerminal, close: closeTerminal } = useTerminal();

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;

      const state = useAppState.getState();

      // Ctrl+Tab / Ctrl+Shift+Tab — cycle tabs
      if (e.key === "Tab") {
        if (state.tabOrder.length === 0) return;
        e.preventDefault();
        e.stopPropagation();
        state.cycleTab(e.shiftKey ? -1 : 1);
        return;
      }

      // Ctrl+Shift+W — close the active tab.
      //
      // Deliberately NOT plain Ctrl+W: that is readline's `kill-word` (delete
      // the previous word), used constantly inside the terminal that is this
      // app's centerpiece. Swallowing it globally would break word-erase in
      // every shell and in Claude Code's own prompt.
      if (e.shiftKey && (e.key === "w" || e.key === "W")) {
        const key = state.activeTabKey;
        if (!key) return;
        e.preventDefault();
        e.stopPropagation();
        if (isTerminalTab(key)) {
          closeTerminal(tabKeyId(key)).catch((err) =>
            console.error("Failed to close terminal:", err),
          );
        } else {
          state.closeHomeTab(tabKeyId(key));
        }
        return;
      }

      // Ctrl+Shift+←/→ — move the active tab, the keyboard route to what
      // dragging a tab does. Shift is what keeps it clear of the terminal:
      // Ctrl+←/→ is readline's word-wise cursor motion.
      //
      // In a text field this chord already means "extend the selection by a
      // word", which is not ours to take: swallowing it would make word-wise
      // selection impossible in every input in the app *and* silently reorder
      // the strip each time someone tried it.
      if (e.shiftKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        if (!state.activeTabKey || inTextField(document.activeElement)) return;
        e.preventDefault();
        e.stopPropagation();
        state.moveActiveTab(e.key === "ArrowLeft" ? -1 : 1);
        return;
      }

      if (e.shiftKey) return;

      // Ctrl+1..9 — jump to tab
      if (/^[1-9]$/.test(e.key)) {
        const index = Number(e.key) - 1;
        if (index >= state.tabOrder.length) return;
        e.preventDefault();
        e.stopPropagation();
        state.focusTabIndex(index);
        return;
      }

      // Ctrl+T — new Claude terminal for the current project
      if (e.key === "t" || e.key === "T") {
        // Prefer the project owning the focused tab, then the sidebar selection.
        const projectId =
          state.sessions.find((s) => s.id === state.activeSessionId)?.projectId ??
          state.selectedProjectId ??
          null;
        const project = state.projects.find((p) => p.id === projectId);
        if (!project || project.status !== "running") return;
        e.preventDefault();
        e.stopPropagation();
        openTerminal(project.id, project.name).catch((err) => {
          state.pushToast({
            kind: "error",
            message: `Could not open a terminal for “${project.name}”`,
            detail: String(err),
          });
        });
        return;
      }

    };

    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [openTerminal, closeTerminal]);
}
