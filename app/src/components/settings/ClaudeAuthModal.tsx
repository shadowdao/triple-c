import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { cancelClaudeToken } from "../../lib/tauri-commands";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import StatusIndicator, { type StatusTone } from "../ui/StatusIndicator";
import { inputClass } from "../ui/Field";
import {
  authErrorMessage,
  useClaudeTokenAcquisition,
} from "../../hooks/useClaudeAuth";
import {
  ANTHROPIC_SIGN_IN_HOSTS,
  sanitizeRelayUrl,
  urlOrigin,
} from "../../lib/urlRelay";

interface Props {
  /** Project whose running container is borrowed to run the CLI. */
  projectId: string;
  projectName: string;
  onClose: () => void;
  /** Fired once the token has been stored, so callers can re-check status. */
  onAuthenticated: () => void;
}

const PHASE_STATUS: Record<string, { tone: StatusTone; label: string }> = {
  waiting: { tone: "busy", label: "Waiting for sign-in" },
  finishing: { tone: "busy", label: "Finishing sign-in" },
  // The CLI refused a code and is back at its prompt. Distinct from "failed":
  // the flow is still live and another code will be accepted.
  rejected: { tone: "error", label: "Code rejected — try again" },
  succeeded: { tone: "ok", label: "Token stored" },
  failed: { tone: "error", label: "Authentication failed" },
};

/**
 * Drives one `claude setup-token` run.
 *
 * The CLI prints a sign-in URL, the user signs in on an Anthropic-hosted page,
 * copies a code from it, and the CLI then blocks on stdin waiting for that
 * code. The input below is the only way to answer that prompt, so it is the
 * centre of this dialog rather than a footnote.
 *
 * Everything shown here is redacted backend-side; the token is never sent to
 * the frontend and is never held in component state.
 */
export default function ClaudeAuthModal({
  projectId,
  projectName,
  onClose,
  onAuthenticated,
}: Props) {
  const flow = useClaudeTokenAcquisition(projectId, onAuthenticated);
  const [code, setCode] = useState("");
  const [copied, setCopied] = useState(false);
  const [linkError, setLinkError] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const codeRef = useRef<HTMLInputElement>(null);
  const outputRef = useRef<HTMLPreElement>(null);

  const running = flow.phase === "running";

  // Cancelling actually aborts the container-side `setup-token` and releases
  // the single-flight guard, so the user can retry immediately. Closing without
  // it would leave the CLI waiting until its 15-minute timeout, blocking any
  // second attempt. Best-effort: if the flow just finished on its own the
  // command is a no-op, and either way the dialog closes.
  const handleCancel = useCallback(() => {
    cancelClaudeToken()
      .catch((e) => console.error("Failed to cancel Claude authentication:", e))
      .finally(onClose);
  }, [onClose]);

  // Follow the tail of the transcript as it streams in.
  useEffect(() => {
    const el = outputRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [flow.output]);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  const status =
    flow.phase === "succeeded"
      ? PHASE_STATUS.succeeded
      : flow.phase === "failed"
        ? PHASE_STATUS.failed
        : flow.codeSubmitted
          ? PHASE_STATUS.finishing
          : flow.codeRejections > 0
            ? PHASE_STATUS.rejected
            : PHASE_STATUS.waiting;

  // Split for display only. `flow.signInUrl` has already passed the host
  // allowlist; this decides which half of it an ellipsis is allowed to eat.
  const signInOrigin = flow.signInUrl ? (urlOrigin(flow.signInUrl) ?? "") : "";
  const signInPath = flow.signInUrl
    ? flow.signInUrl.slice(signInOrigin.length)
    : "";

  const handleOpen = async () => {
    if (!flow.signInUrl) return;
    setLinkError(null);
    // Re-validated at the sink. `extractSignInUrl` already applies the host
    // allowlist, so a failure here means that invariant broke — which is the
    // one moment it matters that the last step before the OS opener checks.
    const target = sanitizeRelayUrl(flow.signInUrl, {
      allowHosts: ANTHROPIC_SIGN_IN_HOSTS,
    });
    if (!target) {
      setLinkError(
        "That link is not an Anthropic sign-in address and was not opened. Start authentication again.",
      );
      return;
    }
    try {
      await openUrl(target);
    } catch (e) {
      setLinkError(
        authErrorMessage(
          e,
          "Could not hand the link to your browser. Copy it and paste it in manually.",
        ),
      );
    }
  };

  const handleCopy = async () => {
    if (!flow.signInUrl) return;
    setLinkError(null);
    // Copying is the manual route to the same browser, so it gets the same
    // check — a link too dangerous to open is too dangerous to hand over.
    const target = sanitizeRelayUrl(flow.signInUrl, {
      allowHosts: ANTHROPIC_SIGN_IN_HOSTS,
    });
    if (!target) {
      setLinkError(
        "That link is not an Anthropic sign-in address and was not copied. Start authentication again.",
      );
      return;
    }
    try {
      await navigator.clipboard.writeText(target);
      setCopied(true);
    } catch (e) {
      setLinkError(
        authErrorMessage(
          e,
          "Could not copy to the clipboard. Select the link text and copy it manually.",
        ),
      );
    }
  };

  const handleSubmitCode = async (e: React.FormEvent) => {
    e.preventDefault();
    const ok = await flow.submitCode(code);
    if (ok) setCode("");
  };

  const latestProgress = flow.progress[flow.progress.length - 1] ?? null;

  return (
    <Modal
      title="Shared Claude authentication"
      description={
        <>
          Running <code className="font-mono">claude setup-token</code> in{" "}
          <strong className="text-[var(--text-primary)]">{projectName}</strong>&rsquo;s
          container. The token it produces is shared by every project.
        </>
      }
      widthClassName="w-[40rem]"
      dismissible={!running}
      onClose={onClose}
      initialFocusRef={codeRef}
      footer={
        confirmCancel ? (
          <>
            <Button size="md" onClick={() => setConfirmCancel(false)}>
              Keep waiting
            </Button>
            <Button size="md" variant="danger" onClick={handleCancel}>
              Cancel sign-in
            </Button>
          </>
        ) : running ? (
          <Button size="md" variant="ghost" onClick={() => setConfirmCancel(true)}>
            Cancel
          </Button>
        ) : (
          <Button
            size="md"
            variant={flow.phase === "succeeded" ? "primary" : "secondary"}
            onClick={onClose}
          >
            {flow.phase === "succeeded" ? "Done" : "Close"}
          </Button>
        )
      }
    >
      <div className="space-y-4">
        <div className="flex items-center justify-between gap-3">
          <StatusIndicator tone={status.tone} label={status.label} className="text-xs" />
          {latestProgress && (
            <p
              data-testid="claude-auth-progress"
              className="min-w-0 flex-1 text-right text-xs text-[var(--text-secondary)] truncate"
              title={latestProgress}
            >
              {latestProgress}
            </p>
          )}
        </div>

        {/* Step 1 — sign in. */}
        <section>
          <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
            1. Sign in with Anthropic
          </h3>
          {flow.signInUrl ? (
            <div className="mt-1 space-y-1.5">
              <div className="flex items-center gap-1.5">
                {/* The origin is rendered at full length and the path is the
                    only part allowed to truncate. A single `truncate` element
                    showing the whole URL is a spoofing primitive: pad the
                    front and the ellipsis eats the half that decides where the
                    user's Anthropic password goes. */}
                <a
                  href={flow.signInUrl}
                  onClick={(e) => {
                    e.preventDefault();
                    void handleOpen();
                  }}
                  className="flex min-w-0 flex-1 items-baseline px-2.5 py-1.5 font-mono text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] transition-colors"
                  title={flow.signInUrl}
                >
                  <span
                    data-testid="claude-auth-url-origin"
                    className="shrink-0 font-semibold [overflow-wrap:anywhere]"
                  >
                    {signInOrigin}
                  </span>
                  <span
                    data-testid="claude-auth-url-path"
                    className="min-w-0 truncate text-[var(--text-secondary)]"
                  >
                    {signInPath}
                  </span>
                </a>
                <Button size="md" onClick={() => void handleOpen()}>
                  Open
                </Button>
                <Button size="md" onClick={() => void handleCopy()}>
                  {copied ? "Copied ✓" : "Copy"}
                </Button>
              </div>
              <p className="text-xs text-[var(--text-secondary)] leading-snug">
                Opens in your normal browser. After signing in, Anthropic shows you a
                code &mdash; copy it and paste it below.
              </p>
            </div>
          ) : (
            <p className="mt-1 text-xs text-[var(--text-secondary)] leading-snug">
              Waiting for <code className="font-mono">claude setup-token</code> to print
              the sign-in link&hellip; It appears in the output below as soon as the CLI
              starts.
            </p>
          )}
          {linkError && (
            <p className="mt-1 text-xs text-[var(--error)]">{linkError}</p>
          )}
        </section>

        {/* Step 2 — the code. Without this the CLI sits on its stdin prompt forever. */}
        <section>
          <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
            2. Paste the code
          </h3>
          <form onSubmit={handleSubmitCode} className="mt-1 flex items-start gap-1.5">
            <div className="min-w-0 flex-1">
              <input
                ref={codeRef}
                type="text"
                value={code}
                onChange={(e) => setCode(e.target.value)}
                disabled={!running || flow.submitting}
                aria-label="Authentication code"
                placeholder="Paste the code from the Anthropic page"
                autoComplete="off"
                spellCheck={false}
                className={`${inputClass} font-mono`}
              />
              {flow.submitError && (
                <p className="mt-1 text-xs text-[var(--error)]">{flow.submitError}</p>
              )}
              {!flow.submitError && flow.codeSubmitted && running && (
                <p className="mt-1 text-xs text-[var(--text-secondary)]">
                  Code sent. Waiting for <code className="font-mono">setup-token</code>{" "}
                  to finish&hellip;
                </p>
              )}
            </div>
            <Button
              size="md"
              variant="primary"
              type="submit"
              disabled={!running || flow.submitting}
            >
              {flow.submitting ? "Sending…" : "Submit code"}
            </Button>
          </form>
        </section>

        {/* Step 3 — outcome. */}
        {flow.phase === "succeeded" && (
          <p
            data-testid="claude-auth-success"
            className="px-2.5 py-2 text-xs text-[var(--success)] bg-[var(--success-muted)] border border-[var(--success)]/40 rounded-[var(--radius-control)]"
          >
            Token stored in the OS keychain. Restart your Anthropic-backend containers
            to start using it.
          </p>
        )}
        {flow.phase === "failed" && flow.error && (
          <p
            data-testid="claude-auth-error"
            className="px-2.5 py-2 text-xs text-[var(--error)] bg-[var(--error-muted)] border border-[var(--error)]/40 rounded-[var(--radius-control)]"
          >
            {flow.error}
          </p>
        )}
        {confirmCancel && (
          <p className="px-2.5 py-2 text-xs text-[var(--warning)] bg-[var(--warning-muted)] border border-[var(--warning)]/40 rounded-[var(--radius-control)] leading-snug">
            This stops <code className="font-mono">claude setup-token</code> inside the
            container and discards the sign-in. No token is stored. You can start again
            straight away.
          </p>
        )}

        {/* Redacted backend-side before it is emitted; still never parsed here. */}
        <section>
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
            Command output
          </h3>
          <pre
            ref={outputRef}
            data-testid="claude-auth-output"
            aria-label="Command output"
            className="mt-1 h-40 overflow-auto whitespace-pre-wrap break-words px-2.5 py-2 font-mono text-[11px] leading-relaxed text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
          >
            {flow.output || "Starting `claude setup-token`…\n"}
          </pre>
        </section>
      </div>
    </Modal>
  );
}
