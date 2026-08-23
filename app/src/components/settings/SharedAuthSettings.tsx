import { useState } from "react";
import Button from "../ui/Button";
import Modal from "../ui/Modal";
import StatusIndicator, { type StatusTone } from "../ui/StatusIndicator";
import { selectClass } from "../ui/Field";
import ClaudeAuthModal from "./ClaudeAuthModal";
import { clearClaudeToken } from "../../lib/tauri-commands";
import type { ClearTokenOutcome } from "../../lib/types";
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
 * `clear_claude_token` reports three separate things about the snapshot images
 * that `docker commit` baked the token into, and they need three different
 * sentences. `snapshots_skipped` in particular is **not** a failure of the
 * rewrite — the project was busy (starting, compacting, migrating) and its
 * snapshot was never attempted, so the remedy is to run the sweep again, not
 * to Reset the project and lose both its volumes.
 *
 * `snapshots_skipped` is read defensively: it is newer than `ClearTokenOutcome`
 * in `lib/types.ts`, which another change in this round owns. Until that lands
 * the field arrives over IPC but is not in the declared type, and an older
 * backend would not send it at all.
 */
type RevokeOutcome = ClearTokenOutcome & { snapshots_skipped?: string[] };

const list = (values: string[] | undefined): string[] => values ?? [];

/** Whether a copy of the token is known — or suspected — to still be reachable. */
function needsAnotherPass(outcome: RevokeOutcome): boolean {
  return (
    list(outcome.snapshots_failed).length > 0 ||
    list(outcome.snapshots_skipped).length > 0 ||
    Boolean(outcome.docker_unavailable)
  );
}

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
  const [sweeping, setSweeping] = useState(false);

  // What the last sweep could not finish. Held in its own state rather than
  // derived from `status`, because that is exactly the bug this fixes: a
  // revoke clears the keychain, `status` flips to "absent", and the button
  // that could have retried the snapshot rewrite disappeared with it —
  // leaving a live ~1-year token in an image and Reset as the only remedy.
  const [leftover, setLeftover] = useState<RevokeOutcome | null>(null);

  // `claude setup-token` runs inside a container, so only running projects can
  // host the flow.
  const runnable = projects.filter(
    (p) => p.status === "running" && p.container_id !== null,
  );
  const host = runnable.find((p) => p.id === pickedId) ?? runnable[0] ?? null;

  const display = STATUS_DISPLAY[status];

  /**
   * Run `clear_claude_token`. It is deliberately the same command for both the
   * first revoke and every retry: it sweeps the snapshot images first, deletes
   * the keychain entry second, and treats a missing entry as success — so
   * calling it again with nothing stored is a pure snapshot sweep, and the
   * images themselves are the durable record of what is left to do.
   */
  const runSweep = async (mode: "revoke" | "sweep") => {
    setSweeping(true);
    try {
      const outcome = (await clearClaudeToken()) as RevokeOutcome;
      setConfirmRevoke(false);
      await refresh();

      const failed = list(outcome.snapshots_failed);
      const skipped = list(outcome.snapshots_skipped);
      const scrubbed = list(outcome.snapshots_scrubbed);
      const superseded = list(outcome.snapshots_superseded);

      setLeftover(needsAnotherPass(outcome) ? outcome : null);

      // The keychain entry is gone either way. What matters here is the copy of
      // the token that `docker commit` baked into each project's snapshot
      // image: that one outlives every container, and `docker image inspect`
      // will keep printing it until the image is rewritten. If that could not
      // be done, the revocation is incomplete and saying "removed" would be a
      // lie.
      if (outcome.docker_unavailable) {
        pushToast({
          kind: "error",
          message:
            mode === "revoke"
              ? "Token removed from the keychain, but snapshots were not checked."
              : "Snapshot images were not checked.",
          detail:
            `Docker could not be reached (${outcome.docker_unavailable}), so any snapshot image ` +
            "built before this version may still contain the token in its environment. " +
            "Start Docker and run the cleanup again to clear them.",
        });
      } else if (failed.length > 0) {
        pushToast({
          kind: "error",
          message: "Token removed from the keychain, but it is still in some images.",
          detail:
            `${failed.length} snapshot image(s) could not be rewritten and ` +
            "still contain the token, readable via `docker image inspect`. Reset those " +
            `projects to remove the images. Details: ${failed.join("; ")}` +
            (skipped.length > 0
              ? ` A further ${skipped.length} image(s) were skipped because their projects ` +
                "are busy; those can be cleared by running the cleanup again."
              : ""),
        });
      } else if (skipped.length > 0) {
        pushToast({
          kind: "error",
          message: `Token still in ${skipped.length} snapshot image(s) — those projects were busy.`,
          detail:
            "Nothing was rewritten for them, so the token is still readable via " +
            "`docker image inspect`. Wait for the operation in progress to finish and run the " +
            `cleanup again. Details: ${skipped.join("; ")}`,
        });
      } else if (scrubbed.length > 0) {
        pushToast({
          kind: "success",
          message:
            mode === "revoke"
              ? `Shared Claude token removed, and cleared from ${scrubbed.length} snapshot image(s).`
              : `Token cleared from ${scrubbed.length} snapshot image(s).`,
          detail:
            superseded.length > 0
              ? "The pre-rewrite image layer for " +
                `${superseded.join(", ")} is still on disk because a ` +
                "container is running from it. It goes away once that project is restarted " +
                "(which recreates the container) and Docker prunes the leftover."
              : undefined,
        });
      } else {
        pushToast({
          kind: "success",
          message:
            mode === "revoke"
              ? "Shared Claude token removed from the keychain."
              : "No snapshot image is holding the token.",
        });
      }
    } catch (e) {
      pushToast({
        kind: "error",
        message:
          mode === "revoke"
            ? "Could not remove the shared Claude token."
            : "Could not clear the token from snapshot images.",
        detail: authErrorMessage(
          e,
          "The OS keychain rejected the delete. The token may still be stored.",
        ),
      });
    } finally {
      setSweeping(false);
    }
  };

  const leftoverFailed = list(leftover?.snapshots_failed);
  const leftoverSkipped = list(leftover?.snapshots_skipped);

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
            disabled={sweeping}
            onClick={() => setConfirmRevoke(true)}
          >
            Revoke
          </Button>
        )}
        {status === "absent" && (
          // Not gated on a stored token, on purpose. A revoke that could not
          // finish leaves the token in a snapshot image while the keychain
          // entry — and therefore the Revoke button — is already gone, and a
          // snapshot committed by an older build carries it whether or not
          // anything is stored today. With nothing in the keychain the same
          // command is a pure image sweep.
          <Button
            size="md"
            variant="ghost"
            disabled={sweeping}
            data-testid="shared-auth-sweep"
            onClick={() => void runSweep("sweep")}
          >
            {sweeping ? "Checking…" : "Check snapshot images"}
          </Button>
        )}
      </div>

      {leftover && (
        <div
          data-testid="shared-auth-leftover"
          className="rounded-[var(--radius-control)] border border-[var(--error)]/40 bg-[var(--error-muted)] p-2"
        >
          <StatusIndicator
            tone="error"
            label="Token still readable"
            className="text-xs"
          />
          <p className="mt-1 text-xs text-[var(--text-secondary)] leading-snug">
            {leftover.docker_unavailable
              ? `Docker could not be reached (${leftover.docker_unavailable}), so no snapshot image was checked.`
              : null}
            {leftoverSkipped.length > 0 ? (
              <>
                {leftoverSkipped.length} snapshot image(s) were skipped because their
                projects were busy. Nothing was rewritten for them, so the token is
                still readable with{" "}
                <code className="font-mono">docker image inspect</code>. Running the
                cleanup again once those projects are idle clears them.
              </>
            ) : null}
            {leftoverFailed.length > 0 ? (
              <>
                {" "}
                {leftoverFailed.length} snapshot image(s) could not be rewritten:{" "}
                {leftoverFailed.join("; ")}. If retrying does not help, Reset those
                projects to remove the images.
              </>
            ) : null}
          </p>
          <div className="mt-2">
            <Button
              size="md"
              variant="secondary"
              disabled={sweeping}
              data-testid="shared-auth-retry"
              onClick={() => void runSweep("sweep")}
            >
              {sweeping ? "Retrying…" : "Retry snapshot cleanup"}
            </Button>
          </div>
        </div>
      )}

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
                disabled={sweeping}
              >
                Cancel
              </Button>
              <Button
                size="md"
                variant="danger"
                disabled={sweeping}
                onClick={() => void runSweep("revoke")}
              >
                {sweeping ? "Revoking…" : "Revoke token"}
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
          <p className="mt-2 text-[13px] text-[var(--text-secondary)] leading-snug">
            Each project&rsquo;s snapshot image is rewritten first, because{" "}
            <code className="font-mono">docker commit</code> copies the token into it
            and an image outlives every container built from it. A project that is
            busy right now is skipped rather than rewritten unsafely &mdash; you will
            be told which, and the cleanup can be run again from here afterwards.
          </p>
        </Modal>
      )}
    </div>
  );
}
