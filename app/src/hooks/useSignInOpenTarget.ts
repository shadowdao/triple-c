import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  checkBrowserViewSupport,
  getAuthBridgeStatus,
} from "../lib/tauri-commands";
import { canOpenPageInContainerBrowser } from "../lib/browserViewSupport";
import type {
  AuthBridgeChangedEvent,
  AuthBridgeStatus,
  PlaywrightDetection,
} from "../lib/types";

/** Emitted by `auth_bridge/mod.rs` whenever the port or conflict set changes. */
const AUTH_BRIDGE_EVENT = "auth-bridge-changed";

/** Which of the URL toast's two buttons should lead for a sign-in link. */
export type SignInOpenTarget = "host" | "container";

/**
 * Whether the auth bridge can be relied on to catch a callback for this
 * project.
 *
 * Deliberately **not** gated on `active_ports` being non-empty. There is only
 * something to bridge once the CLI has bound its callback listener, and the
 * order in which that happens against the URL landing in the transcript is not
 * ours to control — requiring a port here would make the answer depend on a
 * race and flip the default button between two otherwise identical sign-ins.
 * `enabled` is the durable fact: the poller is watching, and it will mirror the
 * port the moment it appears.
 *
 * A conflict is the exception, because it is the one state where the bridge is
 * on and nevertheless *cannot* catch the callback — the host port it needed was
 * already taken. That is precisely when the container-side browser is the
 * better default, so it must not read as live.
 */
export function authBridgeIsLive(status: AuthBridgeStatus | null): boolean {
  if (!status || !status.enabled) return false;
  return status.conflicts.length === 0;
}

/**
 * The rule, as a pure function of the two things it depends on.
 *
 * Both fallbacks land on the host, for different reasons:
 *
 *  - With the bridge live, the host browser is strictly better — it is the
 *    user's own signed-in profile, and the callback still reaches the container.
 *  - With neither available, the host is the *more likely to work* of two
 *    imperfect answers, and it is the one that reports its own failure (see
 *    `handleOpenUrl` in `TerminalView`). The container-side target is
 *    Playwright's dashboard pane, and Playwright's browsers are not baked into
 *    the image, so on a fresh project pointing there fails on every platform
 *    after a several-second wait.
 *
 * Whichever way it goes, both buttons stay in the toast. This chooses which one
 * leads, never which ones exist.
 */
export function chooseSignInTarget(
  bridge: AuthBridgeStatus | null,
  detection: PlaywrightDetection | null,
): SignInOpenTarget {
  if (authBridgeIsLive(bridge)) return "host";
  if (canOpenPageInContainerBrowser(detection)) return "container";
  return "host";
}

/**
 * How long a Playwright probe is reused for.
 *
 * `check_browser_view_support` is a `docker exec` running a Node probe, and
 * every terminal tab of a project would otherwise run its own on mount. Five
 * minutes is long enough that opening a handful of tabs costs one exec, and
 * short enough that pressing "Set up Playwright" in the Browser tab is
 * reflected in the default before the user has finished reading the result.
 */
const DETECTION_TTL_MS = 5 * 60_000;

const detectionCache = new Map<
  string,
  { at: number; probe: Promise<PlaywrightDetection | null> }
>();

/** The shared, rate-limited probe. Never rejects — "didn't answer" is `null`. */
function probeBrowserSupport(projectId: string): Promise<PlaywrightDetection | null> {
  const hit = detectionCache.get(projectId);
  if (hit && Date.now() - hit.at < DETECTION_TTL_MS) return hit.probe;
  const probe = checkBrowserViewSupport(projectId).catch(() => {
    // A failure is usually a stopped container, which is a state the user
    // leaves — so it is not worth remembering for five minutes.
    detectionCache.delete(projectId);
    return null;
  });
  detectionCache.set(projectId, { at: Date.now(), probe });
  return probe;
}

/** Test seam: drops the memoized probes so a case starts from nothing. */
export function resetBrowserSupportCache(): void {
  detectionCache.clear();
}

/**
 * Resolve the default action for Anthropic sign-in links in this project.
 *
 * Resolved at mount rather than when a URL arrives, on purpose: the toast has
 * two buttons side by side, and a default that settles a second after the
 * toast appears moves them under a mouse that is already travelling.
 *
 * The expensive half is only paid when it can change the answer. The bridge
 * status is host-side and cheap; the Playwright probe is a container exec, and
 * a live bridge decides the question before it is ever asked — which, with the
 * bridge now on by default, is the ordinary case.
 */
export function useSignInOpenTarget(projectId: string | undefined): SignInOpenTarget {
  const [target, setTarget] = useState<SignInOpenTarget>("host");

  useEffect(() => {
    if (!projectId) {
      setTarget("host");
      return;
    }

    let cancelled = false;
    let bridge: AuthBridgeStatus | null = null;
    let detection: PlaywrightDetection | null = null;

    const settle = () => {
      if (!cancelled) setTarget(chooseSignInTarget(bridge, detection));
    };

    const consider = (next: AuthBridgeStatus) => {
      bridge = next;
      settle();
      // Only now is the container's side of it worth an exec.
      if (authBridgeIsLive(bridge)) return;
      probeBrowserSupport(projectId).then((d) => {
        if (cancelled) return;
        detection = d;
        settle();
      });
    };

    getAuthBridgeStatus(projectId)
      .then((s) => {
        if (!cancelled) consider(s);
      })
      // Nothing to say to the user here: this only picks which button is
      // filled in, and the fallback is the one that reports its own failures.
      .catch(() => {
        if (!cancelled) consider({ enabled: false, active_ports: [], conflicts: [] });
      });

    // The switch can be flipped *while a login is hanging* — that is the whole
    // reason `set_auth_bridge_enabled` exists outside the Config tab's save —
    // so the default has to follow it rather than reflect whatever was true
    // when this terminal was opened.
    let unlisten: (() => void) | undefined;
    listen<AuthBridgeChangedEvent>(AUTH_BRIDGE_EVENT, (event) => {
      if (event.payload.project_id !== projectId) return;
      consider(event.payload.status);
    })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [projectId]);

  return target;
}
