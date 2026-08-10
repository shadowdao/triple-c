import { useState } from "react";
import Button from "../ui/Button";
import Modal from "../ui/Modal";
import StatusIndicator, { type StatusTone } from "../ui/StatusIndicator";
import { selectClass } from "../ui/Field";
import ClaudeAuthModal from "./ClaudeAuthModal";
import { clearClaudeToken } from "../../lib/tauri-commands";
import { useProjects } from "../../hooks/useProjects";
import { useAppState } from "../../store/appState";
import { authErrorMessage, useClaudeTokenStatus } from "../../hooks/useClaudeAuth";

const STATUS_DISPLAY: Record<
  string,
  { tone: StatusTone; label: string; detail: string }
> = {
  checking: {
    tone: "unknown",
    label: "Checking",
    detail: "Looking for a stored token in the OS keychain.",
  },
  stored: {
    tone: "ok",
    label: "Authenticated",
    detail:
      "A shared token is stored. Anthropic-backend projects use it from their next container start.",
  },
  absent: {
    tone: "off",
    label: "Not authenticated",
    detail:
      "No shared token yet, so each Anthropic-backend project still needs its own `claude login`.",
  },
  unavailable: {
    tone: "error",
    label: "Unknown",
    detail: "The OS keychain could not be read.",
  },
};

/**
 * Host-level control for the one long-lived Claude Code token shared by every
 * project. Acquisition needs a running container to run the CLI in, so the
 * user picks which project lends one.
 */
export default function SharedAuthSettings() {
  const { projects } = useProjects();
  const pushToast = useAppState((s) => s.pushToast);
  const { status, error, refresh } = useClaudeTokenStatus();

  const [pickedId, setPickedId] = useState<string | null>(null);
  const [authOpen, setAuthOpen] = useState(false);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  const [revoking, setRevoking] = useState(false);

  // `claude setup-token` runs inside a container, so only running projects can
  // host the flow.
  const runnable = projects.filter(
    (p) => p.status === "running" && p.container_id !== null,
  );
  const host = runnable.find((p) => p.id === pickedId) ?? runnable[0] ?? null;

  const display = STATUS_DISPLAY[status];

  const handleRevoke = async () => {
    setRevoking(true);
    try {
      await clearClaudeToken();
      setConfirmRevoke(false);
      await refresh();
      pushToast({
        kind: "success",
        message: "Shared Claude token removed from the keychain.",
      });
    } catch (e) {
      pushToast({
        kind: "error",
        message: "Could not remove the shared Claude token.",
        detail: authErrorMessage(
          e,
          "The OS keychain rejected the delete. The token may still be stored.",
        ),
      });
    } finally {
      setRevoking(false);
    }
  };

  return (
    <div className="space-y-3">
      <div>
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-[var(--text-primary)]">
            Shared Claude authentication
          </span>
          <StatusIndicator
            tone={display.tone}
            label={display.label}
            className="text-xs"
          />
        </div>
        <p className="mt-1 text-xs text-[var(--text-secondary)] leading-snug">
          Authenticate once and every project on the Anthropic backend signs in with
          that token, instead of each container running its own{" "}
          <code className="font-mono">claude login</code>. The token is held in your OS
          keychain and injected into containers as an environment variable.
        </p>
        <p
          data-testid="shared-auth-detail"
          className="mt-1 text-xs text-[var(--text-secondary)] leading-snug"
        >
          {display.detail}
        </p>
        {error && <p className="mt-1 text-xs text-[var(--error)]">{error}</p>}
      </div>

      {runnable.length > 1 && (
        <div>
          <label
            htmlFor="shared-auth-host"
            className="block text-xs text-[var(--text-secondary)] mb-1"
          >
            Run the sign-in in
          </label>
          <select
            id="shared-auth-host"
            value={host?.id ?? ""}
            onChange={(e) => setPickedId(e.target.value)}
            className={selectClass}
          >
            {runnable.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
      )}

      <div className="flex items-center gap-2">
        <Button
          size="md"
          variant="primary"
          disabled={!host}
          onClick={() => setAuthOpen(true)}
        >
          {status === "stored" ? "Re-authenticate" : "Authenticate"}
        </Button>
        {status === "stored" && (
          <Button
            size="md"
            variant="danger"
            disabled={revoking}
            onClick={() => setConfirmRevoke(true)}
          >
            Revoke
          </Button>
        )}
      </div>

      {!host && (
        <p
          data-testid="shared-auth-no-container"
          className="text-xs text-[var(--warning)] leading-snug"
        >
          No project is running. Signing in runs{" "}
          <code className="font-mono">claude setup-token</code> inside a container, so
          start a project first &mdash; any one will do, it only lends its container.
        </p>
      )}

      {host && (
        <p className="text-xs text-[var(--text-secondary)] leading-snug">
          The sign-in runs in{" "}
          <strong className="text-[var(--text-primary)]">{host.name}</strong>&rsquo;s
          container, but the resulting token is shared by all projects.
        </p>
      )}

      {authOpen && host && (
        <ClaudeAuthModal
          projectId={host.id}
          projectName={host.name}
          onClose={() => setAuthOpen(false)}
          onAuthenticated={() => {
            void refresh();
          }}
        />
      )}

      {confirmRevoke && (
        <Modal
          title="Revoke shared Claude token"
          widthClassName="w-[26rem]"
          onClose={() => setConfirmRevoke(false)}
          footer={
            <>
              <Button
                size="md"
                variant="ghost"
                onClick={() => setConfirmRevoke(false)}
                disabled={revoking}
              >
                Cancel
              </Button>
              <Button
                size="md"
                variant="danger"
                disabled={revoking}
                onClick={() => void handleRevoke()}
              >
                {revoking ? "Revoking…" : "Revoke token"}
              </Button>
            </>
          }
        >
          <p className="text-[13px] text-[var(--text-secondary)] leading-snug">
            This deletes the shared token from your OS keychain. Anthropic-backend
            projects fall back to their own{" "}
            <code className="font-mono">claude login</code> the next time their
            container starts. Existing running containers keep working until they are
            restarted.
          </p>
        </Modal>
      )}
    </div>
  );
}
