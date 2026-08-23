import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  getAuthBridgeStatus,
  setAuthBridgeEnabled,
} from "../../../../lib/tauri-commands";
import type {
  AuthBridgeChangedEvent,
  AuthBridgeStatus,
  Project,
} from "../../../../lib/types";
import { SwitchRow } from "../../../ui/Field";
import StatusIndicator, { type StatusTone } from "../../../ui/StatusIndicator";
import Toggle from "../../../ui/Toggle";

/** Emitted by `auth_bridge/mod.rs` whenever the port or conflict set changes. */
const AUTH_BRIDGE_EVENT = "auth-bridge-changed";

const LABEL = "Auth bridge";

/**
 * What the indicator beside the switch says.
 *
 * Split out so the interesting part — that a conflict is a *visible* failure —
 * can be tested without a container. Every branch pairs a glyph with a word;
 * none of them are distinguished by colour alone.
 */
export function bridgeIndicator(
  status: AuthBridgeStatus | null,
  containerRunning: boolean,
): { tone: StatusTone; label: string } {
  if (!status) return { tone: "unknown", label: "Checking" };
  if (!status.enabled) return { tone: "off", label: "Off" };
  // A conflict means a login is in progress and its port could not be taken —
  // the one state where doing nothing is the wrong answer, and until now the
  // one state nothing in the app reported at all.
  if (status.conflicts.length > 0) {
    return { tone: "error", label: "Port conflict" };
  }
  if (status.active_ports.some((p) => p.ipv6_warning)) {
    return { tone: "busy", label: "IPv4 only" };
  }
  if (status.active_ports.length > 0) {
    const n = status.active_ports.length;
    return { tone: "running", label: `Bridging ${n} port${n === 1 ? "" : "s"}` };
  }
  // Enabled but holding nothing. Normal: there is only something to bridge
  // while a login is actually waiting for a callback.
  if (!containerRunning) {
    return { tone: "stopped", label: "Waiting for the container" };
  }
  return { tone: "ok", label: "Watching" };
}

/**
 * The switch for `auth_bridge_enabled`, and the only place it can be changed.
 *
 * Two things here are deliberate and easy to undo by accident:
 *
 *  - **It does not go through the Config tab's `save`.** That path is gated on
 *    a stopped container, because almost everything else in the tab is baked
 *    into the container at creation. This is not: the bridge is entirely
 *    host-side, and `set_auth_bridge_enabled` exists precisely so it can be
 *    flipped *while a login is hanging*, which is when the user finds out they
 *    need it. Routing it through the generic save would make it unreachable at
 *    the only moment it matters.
 *  - **It subscribes to `auth-bridge-changed`.** The poller already emits the
 *    bridged-port and conflict sets on every change and, before this, nothing
 *    listened — so a host port the bridge could not take was a completely
 *    silent failure, indistinguishable from a login that simply hung.
 */
export default function AuthBridgeRow({ project }: { project: Project }) {
  const projectId = project.id;
  const containerRunning = project.status === "running";

  const [status, setStatus] = useState<AuthBridgeStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Which write to `status` is the newest — the same "is this still mine?"
   * guard `useContainerMigration` uses around its async
   * writes, and needed here for a reason that is easy to miss.
   *
   * There are two sources of truth for this row and only one of them is
   * ordered. `set_auth_bridge_enabled` resolves with a status *sampled at the
   * moment it returned*; the poller's `auth-bridge-changed` event carries one
   * sampled later. Awaiting the command therefore hands back a value that may
   * already be historical, and writing it unconditionally is how the row ends
   * up saying "Watching" while a port is in fact bound — the failure mode the
   * event subscription exists to prevent, reintroduced one line below it.
   *
   * So every write claims a generation and only lands if it still holds it.
   * A pushed event always claims a fresh one, which is what makes it win over
   * an older awaited result no matter which order the two arrive in.
   */
  const generation = useRef(0);

  useEffect(() => {
    const mine = ++generation.current;
    let cancelled = false;
    setStatus(null);
    setError(null);
    getAuthBridgeStatus(projectId)
      .then((s) => {
        // The initial fetch races the poller exactly like the toggle does: an
        // event can land first and describe a bridge this reply predates.
        if (!cancelled && generation.current === mine) setStatus(s);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<AuthBridgeChangedEvent>(AUTH_BRIDGE_EVENT, (event) => {
      if (event.payload.project_id !== projectId) return;
      // A pushed status is the most recent observation that exists, so it
      // claims the newest generation and invalidates anything still in flight.
      generation.current += 1;
      setStatus(event.payload.status);
    })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch((e) => console.error("Auth bridge event subscription failed:", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [projectId]);

  const toggle = useCallback(
    async (next: boolean) => {
      setBusy(true);
      setError(null);
      // Optimistic, so the switch responds even though enabling has to await a
      // container probe. It claims a generation like every other write, so a
      // pushed event that lands mid-flight supersedes it rather than being
      // undone by the settle below.
      const mine = ++generation.current;
      setStatus((s) => (s ? { ...s, enabled: next } : s));
      try {
        const settled = await setAuthBridgeEnabled(projectId, next);
        // Stale by the time it arrived: the poller has already told us
        // something newer, and `settled` predates it.
        if (generation.current !== mine) return;
        setStatus(settled);
      } catch (e) {
        // The error is reported either way — the command really did fail — but
        // the rollback must not resurrect the pre-toggle value over a status
        // the poller pushed while the command was failing.
        setError(String(e));
        if (generation.current !== mine) return;
        setStatus((s) => (s ? { ...s, enabled: !next } : s));
      } finally {
        setBusy(false);
      }
    },
    [projectId],
  );

  // Fall back to the persisted flag until the first status arrives, so the
  // switch never renders in the wrong position.
  const enabled = status?.enabled ?? project.auth_bridge_enabled;
  const indicator = bridgeIndicator(status, containerRunning);

  return (
    <SwitchRow
      label={LABEL}
      hint={
        <>
          Mirrors a port a program inside the container is listening on onto the
          host's <code>127.0.0.1</code>, so a browser OAuth callback can reach
          the listener waiting inside the container —{" "}
          <code>claude login</code>, <code>aws sso login</code> and{" "}
          <code>gh auth login</code> all work this way, and without it the
          browser calls back into nothing and the login hangs. Host-side only:
          it never recreates the container, and it can be switched on while one
          is running. A bridged port is unauthenticated and reachable by any
          local process for as long as the in-container listener exists, so
          leave it off unless you need it.
          <span className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1">
            <StatusIndicator tone={indicator.tone} label={indicator.label} />
            {status?.active_ports.map((p) => (
              <span
                key={p.port}
                className="font-mono text-[var(--text-secondary)]"
                title={p.ipv6_warning ?? `Bound on host 127.0.0.1:${p.port}`}
              >
                127.0.0.1:{p.port}
                {p.ipv6_warning ? " (IPv4 only)" : ""}
              </span>
            ))}
          </span>
          {status?.conflicts.map((c) => (
            <span
              key={c.port}
              className="mt-1 block text-[var(--error)]"
              role="status"
            >
              Port {c.port}: {c.reason}
            </span>
          ))}
          {status?.active_ports
            .filter((p) => p.ipv6_warning)
            .map((p) => (
              <span key={p.port} className="mt-1 block text-[var(--warning)]">
                Port {p.port}: {p.ipv6_warning}
              </span>
            ))}
          {error && (
            <span className="mt-1 block text-[var(--error)]">{error}</span>
          )}
        </>
      }
      control={
        <Toggle
          label={LABEL}
          checked={enabled}
          // Never gated on the container being stopped — see the note above.
          disabled={busy}
          onChange={toggle}
        />
      }
    />
  );
}
