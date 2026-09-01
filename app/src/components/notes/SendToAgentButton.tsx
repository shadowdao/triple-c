import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState, terminalTabKey } from "../../store/appState";
import { toClaudePayload } from "../../lib/claudeInput";
import { sessionDisplayName } from "../../lib/sessionName";
import Button from "../ui/Button";

interface Props {
  projectId: string;
  body: string;
}

/**
 * Puts a note into a running Claude session's prompt.
 *
 * Three behaviours by target count: none disables the button, one sends
 * straight there, several ask which. It never guesses — the note goes to a
 * session the user named, or to the only one there is.
 *
 * Only `claude` sessions are offered. A bash tab would receive ESC+CR as an
 * unbound readline key and answer with a bell (see `lib/claudeInput.ts`).
 */
export default function SendToAgentButton({ projectId, body }: Props) {
  const { sessions, sendInput } = useTerminal();
  const { projects, setActiveTabKey, pushToast } = useAppState(
    useShallow((s) => ({
      projects: s.projects,
      setActiveTabKey: s.setActiveTabKey,
      pushToast: s.pushToast,
    })),
  );
  const [menuOpen, setMenuOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  const targets = useMemo(
    () =>
      sessions.filter(
        (s) => s.projectId === projectId && s.sessionType === "claude",
      ),
    [sessions, projectId],
  );

  const project = projects.find((p) => p.id === projectId);
  const hasBody = body.trim().length > 0;
  const disabled = targets.length === 0 || !hasBody;

  // Same dismissal contract as `ui/OverflowMenu` and the tab context menu.
  useEffect(() => {
    if (!menuOpen) return;
    const onDocClick = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setMenuOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  const send = useCallback(
    async (sessionId: string) => {
      setMenuOpen(false);
      try {
        // No trailing CR: the note lands in the prompt and the user presses
        // Enter. Newlines become ESC+CR so it arrives as one message rather
        // than one prompt per line.
        await sendInput(sessionId, toClaudePayload(body));
        // A courtesy, not part of the send: if the tab cannot be focused the
        // text still went.
        setActiveTabKey(terminalTabKey(sessionId));
      } catch (e) {
        pushToast({
          kind: "error",
          message: "Could not send the note to the agent",
          detail: String(e),
        });
      }
    },
    [body, sendInput, setActiveTabKey, pushToast],
  );

  const onClick = useCallback(() => {
    // The target is resolved at click time and pinned for the whole send, the
    // hazard `useSTT` guards against by capturing its session at record start:
    // the list can change while the request is in flight.
    if (targets.length === 1) {
      void send(targets[0].id);
      return;
    }
    setMenuOpen((open) => !open);
  }, [targets, send]);

  const title = !hasBody
    ? "Nothing to send — this note is empty"
    : targets.length === 0
      ? "No running Claude session for this project"
      : "Put this note into the agent's prompt (you press Enter)";

  return (
    <div ref={rootRef} className="relative inline-block">
      <Button
        variant="secondary"
        disabled={disabled}
        onClick={onClick}
        aria-haspopup={targets.length > 1 ? "menu" : undefined}
        aria-expanded={targets.length > 1 ? menuOpen : undefined}
        title={title}
      >
        Send to agent
      </Button>
      {menuOpen && targets.length > 1 && (
        <div
          role="menu"
          className="absolute right-0 z-40 mt-1 min-w-[12rem] py-1 bg-[var(--bg-overlay)] border border-[var(--border-color)] rounded-[var(--radius-panel)] text-xs"
          style={{ boxShadow: "var(--shadow-overlay)" }}
        >
          {targets.map((s) => (
            <button
              key={s.id}
              type="button"
              role="menuitem"
              onClick={() => void send(s.id)}
              className="w-full text-left px-3 py-1.5 text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] transition-colors"
            >
              {sessionDisplayName(s, project)}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
