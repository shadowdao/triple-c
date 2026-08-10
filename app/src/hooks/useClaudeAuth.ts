import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as commands from "../lib/tauri-commands";
import { ANTHROPIC_SIGN_IN_HOSTS, sanitizeRelayUrl } from "../lib/urlRelay";
import type {
  ClaudeTokenOutputEvent,
  ClaudeTokenProgressEvent,
} from "../lib/types";

/**
 * Front-end half of the shared Claude Code token flow.
 *
 * The token itself never crosses the IPC boundary — `has_claude_token` returns
 * a boolean and the streamed output is redacted backend-side. Nothing in here
 * stores, parses, or renders a credential; the transcript is displayed as-is
 * precisely because it has already been scrubbed.
 */

/** Emitted by `auth_token_commands.rs`; payload shapes live in `lib/types.ts`. */
const PROGRESS_EVENT = "claude-token-progress";
const OUTPUT_EVENT = "claude-token-output";

/** Bound on the retained transcript. The tail is the interesting part. */
const MAX_OUTPUT = 64 * 1024;

/**
 * Tauri rejects an `invoke` with the Rust `Err(String)` itself, and this
 * backend writes its errors as complete, actionable sentences ("The container
 * for 'x' is not running. Start it, then run authentication again."). So use
 * them verbatim rather than stringifying an opaque value, and only synthesise
 * a message when the rejection is something else — a thrown `Error`, or an IPC
 * channel that died without one.
 */
export function authErrorMessage(e: unknown, fallback: string): string {
  if (typeof e === "string" && e.trim()) return e.trim();
  if (e instanceof Error && e.message.trim()) return e.message.trim();
  return fallback;
}

/**
 * Pick the sign-in URL out of `claude setup-token`'s transcript.
 *
 * **The transcript is container output, so every candidate here is
 * attacker-controlled if the sandboxed agent misbehaves.** It is then rendered
 * under a heading that says "Sign in with Anthropic" and handed to the host
 * browser, which makes this the highest-value URL in the app to spoof: a user
 * who follows it types their real Anthropic credentials into whatever it
 * resolves to. Three rules follow, and none of them are optional:
 *
 *  - Every candidate goes through the shared {@link sanitizeRelayUrl}, with a
 *    host allowlist. Only Anthropic's own domains can be a sign-in link;
 *    userinfo (`https://claude.ai@evil.tld/...`) and control characters are
 *    rejected there.
 *  - The **first** surviving candidate wins. The previous rule was
 *    longest-wins, which handed the choice to the attacker: pad a hostile URL
 *    and it displaces the real one that came before it.
 *  - The one exception is a candidate that *extends* the current pick, i.e.
 *    starts with it. That is the case longest-wins existed for — a repainting
 *    TUI can land a truncated copy of the same link in the transcript before
 *    the complete one — and it cannot swap the origin, because a longer string
 *    with the same prefix has the same host.
 */
export function extractSignInUrl(text: string): string | null {
  // eslint-disable-next-line no-control-regex
  const matches = text.match(/https?:\/\/[^\s"'`<>\x00-\x20\x7f]+/g);
  if (!matches) return null;

  const cleaned = matches
    // Trailing punctuation belongs to the prose, not the URL.
    .map((url) => url.replace(/[.,;:!?)\]}>'"]+$/, ""))
    .map((url) => sanitizeRelayUrl(url, { allowHosts: ANTHROPIC_SIGN_IN_HOSTS }))
    .filter((url): url is string => url !== null);

  const oauth = cleaned.filter((url) => /oauth|authorize|login/i.test(url));
  const pool = oauth.length > 0 ? oauth : cleaned;

  let best: string | null = null;
  for (const url of pool) {
    if (best === null || url.startsWith(best)) best = url;
  }
  return best;
}

// ─────────────────────────────────────────────────────────────────────────────
// Token presence
// ─────────────────────────────────────────────────────────────────────────────

export type ClaudeTokenStatus = "checking" | "stored" | "absent" | "unavailable";

/** Whether a shared token exists, plus a way to re-check after a change. */
export function useClaudeTokenStatus() {
  const [status, setStatus] = useState<ClaudeTokenStatus>("checking");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const present = await commands.hasClaudeToken();
      setStatus(present ? "stored" : "absent");
      setError(null);
    } catch (e) {
      setStatus("unavailable");
      setError(
        authErrorMessage(
          e,
          "Could not read the OS keychain, so whether a shared token exists is unknown.",
        ),
      );
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return { status, error, refresh };
}

// ─────────────────────────────────────────────────────────────────────────────
// Acquisition
// ─────────────────────────────────────────────────────────────────────────────

export type AcquisitionPhase = "running" | "succeeded" | "failed";

export interface ClaudeTokenAcquisition {
  phase: AcquisitionPhase;
  /** Milestone messages from `claude-token-progress`, oldest first. */
  progress: string[];
  /** Redacted transcript from `claude-token-output`. */
  output: string;
  signInUrl: string | null;
  /** Set when the flow ends badly; always a full sentence the user can act on. */
  error: string | null;
  submitting: boolean;
  codeSubmitted: boolean;
  submitError: string | null;
  submitCode: (code: string) => Promise<boolean>;
}

/**
 * Runs one `acquire_claude_token` flow for the lifetime of the calling
 * component. Starts on mount, so mount this only when the user has asked for
 * it — the backend allows a single flow at a time.
 *
 * `onSucceeded` fires once, after the token has been stored.
 */
export function useClaudeTokenAcquisition(
  projectId: string,
  onSucceeded?: () => void,
): ClaudeTokenAcquisition {
  const [phase, setPhase] = useState<AcquisitionPhase>("running");
  const [progress, setProgress] = useState<string[]>([]);
  const [output, setOutput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [codeSubmitted, setCodeSubmitted] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  // Held in a ref so a fresh callback identity cannot restart the flow.
  const succeededRef = useRef(onSucceeded);
  succeededRef.current = onSucceeded;

  useEffect(() => {
    let cancelled = false;
    const unlisteners: UnlistenFn[] = [];

    const register = async <T,>(name: string, handle: (payload: T) => void) => {
      const unlisten = await listen<T>(name, (event) => handle(event.payload));
      // Registration is async: if the component went away while we were
      // awaiting, drop the listener now rather than leaking it.
      if (cancelled) {
        unlisten();
        return;
      }
      unlisteners.push(unlisten);
    };

    void (async () => {
      try {
        await register<ClaudeTokenProgressEvent>(PROGRESS_EVENT, (payload) => {
          if (payload.project_id !== projectId) return;
          setProgress((prev) =>
            prev[prev.length - 1] === payload.message
              ? prev
              : [...prev, payload.message],
          );
        });
        await register<ClaudeTokenOutputEvent>(OUTPUT_EVENT, (payload) => {
          if (payload.project_id !== projectId) return;
          setOutput((prev) => {
            const next = prev + payload.chunk;
            return next.length > MAX_OUTPUT
              ? next.slice(next.length - MAX_OUTPUT)
              : next;
          });
        });
      } catch (e) {
        if (cancelled) return;
        setPhase("failed");
        setError(
          authErrorMessage(
            e,
            "Could not subscribe to the authentication events, so the flow was not started. Restart Triple-C and try again.",
          ),
        );
        return;
      }

      if (cancelled) return;

      try {
        await commands.acquireClaudeToken(projectId);
        if (cancelled) return;
        setPhase("succeeded");
        succeededRef.current?.();
      } catch (e) {
        if (cancelled) return;
        setPhase("failed");
        setError(
          authErrorMessage(
            e,
            "`claude setup-token` did not finish. No token was stored — try again.",
          ),
        );
      }
    })();

    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) {
        try {
          unlisten();
        } catch {
          // Nothing useful to do while tearing down.
        }
      }
    };
  }, [projectId]);

  const submitCode = useCallback(async (code: string) => {
    const trimmed = code.trim();
    if (!trimmed) {
      setSubmitError("Enter the code shown after signing in.");
      return false;
    }
    setSubmitting(true);
    setSubmitError(null);
    try {
      await commands.submitClaudeTokenCode(trimmed);
      setCodeSubmitted(true);
      return true;
    } catch (e) {
      setSubmitError(
        authErrorMessage(
          e,
          "Could not deliver the code to `claude setup-token`. Copy it again and retry.",
        ),
      );
      return false;
    } finally {
      setSubmitting(false);
    }
  }, []);

  const signInUrl = useMemo(() => extractSignInUrl(output), [output]);

  return {
    phase,
    progress,
    output,
    signInUrl,
    error,
    submitting,
    codeSubmitted,
    submitError,
    submitCode,
  };
}
