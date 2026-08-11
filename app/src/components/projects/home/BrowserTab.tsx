import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  BrowserInstallTarget,
  BrowserSetupOutcome,
  BrowserViewChangedEvent,
  BrowserViewPopoutChangedEvent,
  BrowserViewStatus,
  PlaywrightDetection,
  Project,
} from "../../../lib/types";
import {
  checkBrowserViewSupport,
  closeBrowserViewPopout,
  getBrowserViewStatus,
  installBrowserViewBrowser,
  installBrowserViewSupport,
  getBrowserViewMatchWindow,
  getBrowserViewPopoutState,
  openBrowserViewPopout,
  openPageInContainerBrowser,
  setBrowserViewEnabled,
  setBrowserViewMatchWindow,
  setBrowserViewPopoutAlwaysOnTop,
} from "../../../lib/tauri-commands";
import { useAppState } from "../../../store/appState";
import OpenPageDialog from "./OpenPageDialog";
import AccordionSection from "../../ui/AccordionSection";
import Button from "../../ui/Button";
import StatusIndicator from "../../ui/StatusIndicator";
import Toggle from "../../ui/Toggle";

interface Props {
  project: Project;
  active: boolean;
}

const OFF: BrowserViewStatus = {
  enabled: false,
  state: "off",
  url: null,
  host_port: null,
  container_port: null,
  started_at: null,
  detection: null,
  message: null,
};

/** Which install is in flight. `null` means none — nothing installs itself. */
type SetupJob = null | "packages" | BrowserInstallTarget;

/**
 * Watch — and take over — the browser Claude is driving with Playwright inside
 * the container.
 *
 * The pane is an iframe onto Playwright's own live dashboard, which runs in the
 * container and is reached through a token-gated listener on the host's
 * loopback. Nothing starts until the user asks: this is remote control of a
 * browser in a privileged sandbox, so it is off by default and opted into per
 * project, exactly like the auth bridge.
 *
 * The same rule, harder, applies to setup. Opening this tab *probes* the
 * container (one `node -e`, read-only) so the pane can say what is missing
 * before the user asks for a view — but it never installs anything. Installing
 * packages and downloading a browser are container mutations measured in
 * hundreds of megabytes; both are separate, labelled, user-pressed buttons.
 */
export default function BrowserTab({ project, active }: Props) {
  const [status, setStatus] = useState<BrowserViewStatus>(OFF);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Bumped to force the iframe to reload without changing its src. */
  const [reloadKey, setReloadKey] = useState(0);
  /** Last read-only probe of the container, for the setup panel. */
  const [detection, setDetection] = useState<PlaywrightDetection | null>(null);
  const [job, setJob] = useState<SetupJob>(null);
  const [outcome, setOutcome] = useState<BrowserSetupOutcome | null>(null);
  const [setupError, setSetupError] = useState<string | null>(null);
  /**
   * Whether the view is in its own window instead of this pane, and whether
   * that window is pinned. `null` means "not asked yet" — a distinct state from
   * "not popped out", because rendering the iframe on a guess is what puts a
   * second viewer on the browser.
   */
  const [poppedOut, setPoppedOut] = useState<boolean | null>(null);
  const [onTop, setOnTop] = useState(false);
  /** The "open a page" dialog, and the request it is running. */
  const [matchWindow, setMatchWindow] = useState(false);
  const [askPage, setAskPage] = useState(false);
  const [openingPage, setOpeningPage] = useState(false);
  const pushToast = useAppState((s) => s.pushToast);
  const setContainerProgress = useAppState((s) => s.setContainerProgress);
  const progress = useAppState((s) => s.containerProgress[project.id]);
  const running = project.status === "running";

  // The backend is the source of truth: it emits whenever a view starts or is
  // torn down (container stopped, project removed, viewer died).
  const projectId = project.id;
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    let dispose: (() => void) | undefined;
    listen<BrowserViewChangedEvent>("browser-view-changed", (event) => {
      if (event.payload.project_id === projectId && mounted.current) {
        setStatus(event.payload.status);
      }
    }).then((un) => {
      if (mounted.current) dispose = un;
      else un();
    });
    return () => dispose?.();
  }, [projectId]);

  // The window is the backend's, not this component's: it survives the tab
  // being closed, the pane being unmounted and the view being torn down from
  // elsewhere. So its state is listened for, never assumed.
  useEffect(() => {
    let dispose: (() => void) | undefined;
    listen<BrowserViewPopoutChangedEvent>("browser-view-popout-changed", (event) => {
      if (event.payload.project_id === projectId && mounted.current) {
        setPoppedOut(event.payload.open);
        setOnTop(event.payload.always_on_top);
      }
    }).then((un) => {
      if (mounted.current) dispose = un;
      else un();
    });
    return () => dispose?.();
  }, [projectId]);

  useEffect(() => {
    if (!active || !running) return;
    getBrowserViewPopoutState(projectId)
      .then((s) => {
        if (!mounted.current) return;
        setPoppedOut(s.open);
        setOnTop(s.always_on_top);
      })
      // Unreachable in practice, but a pane stuck at "not asked yet" would
      // never show the view at all — so fail towards the tab.
      .catch(() => mounted.current && setPoppedOut(false));
    getBrowserViewMatchWindow(projectId)
      .then((on) => mounted.current && setMatchWindow(on))
      .catch(() => {});
    getBrowserViewStatus(projectId)
      .then((s) => mounted.current && setStatus(s))
      .catch(() => {});
    // Read-only. This is what lets the pane offer setup before the user hits a
    // wall, and it is why a "not installed" answer is never stale.
    checkBrowserViewSupport(projectId)
      .then((d) => mounted.current && setDetection(d))
      .catch(() => {});
  }, [active, projectId, running]);

  const toggle = useCallback(
    async (next: boolean) => {
      setBusy(true);
      setError(null);
      try {
        const result = await setBrowserViewEnabled(projectId, next);
        if (mounted.current) setStatus(result);
      } catch (e) {
        const detail = String(e);
        if (mounted.current) setError(detail);
        pushToast({
          kind: "error",
          message: next ? "Could not start the browser view" : "Could not stop the browser view",
          detail,
        });
      } finally {
        if (mounted.current) setBusy(false);
      }
    },
    [projectId, pushToast],
  );

  /**
   * Pop the view out, or pull it back.
   *
   * Both are window operations only — the viewer keeps running either way — so
   * this is cheap enough to toggle freely and never interrupts what the agent
   * is doing in the browser.
   */
  const popOut = useCallback(async () => {
    try {
      await openBrowserViewPopout(projectId, onTop);
      if (mounted.current) setPoppedOut(true);
    } catch (e) {
      pushToast({
        kind: "error",
        message: "Could not open the browser in its own window",
        detail: String(e),
      });
    }
  }, [projectId, onTop, pushToast]);

  const popIn = useCallback(async () => {
    try {
      await closeBrowserViewPopout(projectId);
      if (mounted.current) setPoppedOut(false);
    } catch (e) {
      pushToast({
        kind: "error",
        message: "Could not close the browser window",
        detail: String(e),
      });
    }
  }, [projectId, pushToast]);

  const toggleOnTop = useCallback(
    async (next: boolean) => {
      setOnTop(next);
      try {
        await setBrowserViewPopoutAlwaysOnTop(projectId, next);
      } catch (e) {
        if (mounted.current) setOnTop(!next);
        pushToast({
          kind: "error",
          message: "Could not change the window's stacking",
          detail: String(e),
        });
      }
    },
    [projectId, pushToast],
  );

  /**
   * Open a URL in a browser inside the container.
   *
   * The pane only ever *watched* browsers something else published; this is the
   * one action that opens one. It also means the page can be resized later —
   * whoever launches a bound browser is the only process that can drive it.
   */
  const openPage = useCallback(
    async (url: string, width: number, height: number) => {
      setOpeningPage(true);
      try {
        const result = await openPageInContainerBrowser(projectId, url, width, height);
        if (!mounted.current) return;
        setAskPage(false);
        if (result.error) {
          pushToast({ kind: "error", message: "The page didn’t open", detail: result.error });
        } else {
          pushToast({ kind: "success", message: `Opened ${url} at ${width}×${height}` });
        }
      } catch (e) {
        pushToast({
          kind: "error",
          message: "Could not open the page in the container’s browser",
          detail: String(e),
        });
      } finally {
        if (mounted.current) setOpeningPage(false);
      }
    },
    [projectId, pushToast],
  );

  const toggleMatchWindow = useCallback(
    async (next: boolean) => {
      setMatchWindow(next);
      try {
        await setBrowserViewMatchWindow(projectId, next);
      } catch (e) {
        if (mounted.current) setMatchWindow(!next);
        pushToast({
          kind: "error",
          message: "Could not match the page to the window",
          detail: String(e),
        });
      }
    },
    [projectId, pushToast],
  );

  /** Run one install. Every path clears the progress line it started. */
  const install = useCallback(
    async (which: Exclude<SetupJob, null>) => {
      setJob(which);
      setSetupError(null);
      setOutcome(null);
      try {
        const result =
          which === "packages"
            ? await installBrowserViewSupport(projectId)
            : await installBrowserViewBrowser(projectId, which);
        if (!mounted.current) return;
        // The command re-probes, so the pane updates itself — no reopening the
        // tab, no second button to press.
        setDetection(result.detection);
        setOutcome(result);
        if (result.warning) {
          // Not an error — the step did what it said — but the caveat is the
          // part that decides whether the browser will actually work.
          pushToast({
            kind: "info",
            message: "Setup finished, with something to know",
            detail: result.warning,
          });
        } else {
          pushToast({
            kind: "success",
            message:
              which === "packages" ? "Playwright installed" : `${which} installed and verified`,
          });
        }
      } catch (e) {
        const detail = String(e);
        if (mounted.current) setSetupError(detail);
        pushToast({ kind: "error", message: "Setup failed", detail });
      } finally {
        setContainerProgress(projectId, null);
        if (mounted.current) setJob(null);
      }
    },
    [projectId, pushToast, setContainerProgress],
  );

  // A stopped container can't be hosting a browser — and can't be installed
  // into either, so say that plainly rather than offering controls that would
  // only fail.
  if (!running) {
    return (
      <Explainer title="The container isn’t running.">
        Start the container, have Claude drive a browser with Playwright, then come
        back here to watch it.
      </Explainer>
    );
  }

  const live = status.state === "running" && status.url;
  // Prefer the probe: it is the fresher of the two, and it is the one that
  // reflects an install that just finished.
  const probed = detection ?? status.detection;
  const ready = isUsable(probed);
  // Mirrors Rust `PlaywrightDetection::needs_browser`: the Chrome channel is an
  // apt package, so it never shows up in `browsers`, and a container that has
  // it is not missing a browser.
  const needsBrowser =
    probed !== null &&
    probed.chrome_channel === null &&
    (probed.browsers.length === 0 || revisionSkew(probed));
  const needsSetup = probed !== null && (!ready || needsBrowser);

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-center gap-2 px-4 py-2 border-b border-[var(--border-color)] flex-shrink-0 flex-wrap">
        <StatusIndicator
          tone={
            busy
              ? "busy"
              : status.state === "running"
                ? "running"
                : status.state === "unavailable"
                  ? "error"
                  : "off"
          }
          label={
            busy
              ? "Starting"
              : status.state === "running"
                ? "Live"
                : status.state === "unavailable"
                  ? "Unavailable"
                  : "Off"
          }
        />
        {live && (
          <span className="text-xs text-[var(--text-secondary)] font-mono truncate">
            127.0.0.1:{status.host_port} → container :{status.container_port}
          </span>
        )}
        <div className="flex-1" />
        {live && poppedOut === true && (
          <span className="flex items-center gap-1.5 text-xs text-[var(--text-secondary)]">
            Keep on top
            {/* The accessible name matches the visible text, as everywhere else
                a Toggle is used — a `<label>` around it would be inert anyway,
                since a Toggle renders a button. */}
            <Toggle checked={onTop} onChange={toggleOnTop} label="Keep on top" />
          </span>
        )}
        {live && poppedOut === true && (
          <span
            className="flex items-center gap-1.5 text-xs text-[var(--text-secondary)]"
            title="Resize the page itself as the window is dragged, so the layout actually reflows. Applies to pages opened from here."
          >
            Match window
            <Toggle checked={matchWindow} onChange={toggleMatchWindow} label="Match window" />
          </span>
        )}
        {live && poppedOut === false && (
          <Button size="md" onClick={() => setReloadKey((k) => k + 1)}>
            Reload
          </Button>
        )}
        {live && (
          <Button size="md" onClick={() => setAskPage(true)}>
            Open a page…
          </Button>
        )}
        {live && poppedOut !== null && (
          <Button size="md" onClick={poppedOut ? popIn : popOut}>
            {poppedOut ? "Put back in tab" : "Open in own window"}
          </Button>
        )}
        <Button
          size="md"
          variant={live ? "secondary" : "primary"}
          disabled={busy || job !== null}
          onClick={() => toggle(!status.enabled || status.state !== "running")}
        >
          {busy ? "Working…" : live ? "Stop" : "Start browser view"}
        </Button>
      </div>

      {live && poppedOut === true ? (
        // The iframe is unmounted while the window is up, on purpose. Two
        // viewers on one browser both work, but both also *drive* it — two
        // cursors taking over the same page is not a feature.
        <div className="flex-1 min-h-0 flex items-center justify-center p-6">
          <div className="max-w-[28rem] text-center">
            <h2 className="text-[13px] font-semibold text-[var(--text-primary)]">
              This view is in its own window.
            </h2>
            <p className="mt-1 text-[13px] text-[var(--text-secondary)] leading-relaxed">
              Move it to another screen, or keep it on top, and watch the browser while
              you work here. The view keeps running either way — closing the window
              brings it back into this tab.
            </p>
            <div className="mt-3 flex items-center justify-center gap-2">
              <Button size="md" variant="primary" onClick={popIn}>
                Put back in tab
              </Button>
            </div>
          </div>
        </div>
      ) : live && poppedOut === false ? (
        <iframe
          key={reloadKey}
          // Loopback only, and the URL carries the one-time session token the
          // host-side gate checks before anything reaches the container.
          src={status.url ?? undefined}
          title={`Playwright browser view for ${project.name}`}
          className="flex-1 min-h-0 w-full border-0 bg-[var(--bg-primary)]"
        />
      ) : live ? (
        // Live, but the window's state hasn't come back yet. An instant, and
        // deliberately empty: guessing "not popped out" here is what would
        // flash a second viewer onto the browser.
        <div className="flex-1 min-h-0" />
      ) : (
        <div className="flex-1 min-h-0 overflow-y-auto">
          {/* Setup stays on screen while an install is running and after it
              finishes, so its output and caveats don't vanish at the moment
              they become readable. */}
          {needsSetup ||
          status.state === "unavailable" ||
          job !== null ||
          outcome !== null ||
          setupError !== null ? (
            <Setup
              detection={probed}
              message={status.state === "unavailable" ? status.message : null}
              job={job}
              progress={job ? progress : undefined}
              outcome={outcome}
              error={setupError}
              onInstall={install}
            />
          ) : error ? (
            <Explainer title="The browser view didn’t start." tone="error">
              <span className="font-mono text-xs break-words">{error}</span>
            </Explainer>
          ) : (
            <Explainer title="Nothing is being watched yet.">
              Start the view to run Playwright’s live dashboard inside this container
              and mirror it here. You’ll see any browser a script has published with{" "}
              <Code>await browser.bind(&apos;claude&apos;)</Code> — and{" "}
              <Code>@playwright/mcp</Code> publishes automatically, so nothing extra is
              needed if Claude is using that.
            </Explainer>
          )}
        </div>
      )}

      {askPage && (
        <OpenPageDialog
          busy={openingPage}
          onOpen={openPage}
          onClose={() => setAskPage(false)}
        />
      )}
    </div>
  );
}

/** Mirrors Rust `PlaywrightDetection::is_usable`. */
function isUsable(d: PlaywrightDetection | null): boolean {
  return d !== null && d.playwright_version !== null && d.has_bind && d.cli_entry !== null;
}

/**
 * Mirrors Rust `PlaywrightDetection::revision_skew`.
 *
 * Browsers are installed, but not the revision one of the two Playwright copies
 * would launch — so the cache looks full and launches fail. A probe that didn't
 * answer leaves the executable null, and "unknown" must not read as "broken".
 */
function revisionSkew(d: PlaywrightDetection | null): boolean {
  if (!d || d.browsers.length === 0) return false;
  // `!= null`, not `!== null`: a probe from a container that predates these
  // fields omits them entirely, and `undefined` is "didn't answer" — which must
  // never render as "your browsers are wrong".
  const viewerBroken = d.chromium_executable != null && !d.chromium_executable_exists;
  const scriptsBroken =
    d.script_chromium_executable != null && !d.script_chromium_executable_exists;
  return viewerBroken || scriptsBroken;
}

/**
 * The skew sentence, naming both halves.
 *
 * "Install a browser" over a cache that visibly already holds one reads as
 * nonsense, so the copy has to say which copy of Playwright wants what.
 */
function skewText(d: PlaywrightDetection | null): string {
  if (!d) return "";
  const scriptsBroken =
    d.script_chromium_executable !== null && !d.script_chromium_executable_exists;
  const [version, wanted] = scriptsBroken
    ? [d.script_playwright_version, d.script_chromium_executable]
    : [d.playwright_version, d.chromium_executable];
  return (
    `This container has ${d.browsers.join(", ")}, but ` +
    `${scriptsBroken ? 'the Playwright a script gets from require("playwright")' : "the Playwright serving the viewer"}` +
    ` — ${version ?? "?"} — launches ${wanted ?? "?"}, which isn’t there. ` +
    (scriptsBroken
      ? "Two copies ended up in one tree, each pinning its own browser revision, so the viewer works and every script Claude writes fails. Re-run “Set up Playwright” to reinstall them as one consistent set."
      : "Install Chromium below: it runs that build’s own installer, so it fetches exactly the revision that is missing.")
  );
}

/** What the container is short of, as a list rather than as prose. */
function missingParts(d: PlaywrightDetection | null): string[] {
  if (!d) return [];
  const out: string[] = [];
  if (!d.node_version) out.push("Node.js");
  if (!d.playwright_version) out.push("playwright");
  else if (!d.has_bind) out.push("a newer playwright — this build has no browser.bind()");
  if (!d.cli_entry) out.push("@playwright/cli");
  return out;
}

/**
 * Setup, as one action per line, each saying what it costs before it is
 * pressed.
 *
 * The old pane printed npm commands here and left the rest to the user. The
 * result, verified with a real one: an `@playwright/mcp` install that could
 * never satisfy this pane, a global install that hit EACCES, a Chromium that
 * downloaded and then would not start because the image shipped none of its
 * shared libraries, and a long tail of commands after that. Current base images
 * bake those libraries in, so that last one is fixed at the source — but a
 * project keeps its original base image until it is migrated, so the install
 * action still handles a container that lacks them.
 */
function Setup({
  detection,
  message,
  job,
  progress,
  outcome,
  error,
  onInstall,
}: {
  detection: PlaywrightDetection | null;
  message: string | null;
  job: SetupJob;
  progress?: string;
  outcome: BrowserSetupOutcome | null;
  error: string | null;
  onInstall: (which: Exclude<SetupJob, null>) => void;
}) {
  const busy = job !== null;
  const havePackages = isUsable(detection);
  const missing = missingParts(detection);
  const browsers = detection?.browsers ?? [];
  const chrome = detection?.chrome_channel ?? null;
  const noBrowser = browsers.length === 0 && chrome === null;
  // Installed browsers that cannot be launched. Handled apart from `noBrowser`
  // because the fix is the same button but the sentence must not be "install a
  // browser" over a cache that visibly has one.
  const skew = revisionSkew(detection) && chrome === null;

  return (
    <div className="p-4 max-w-[46rem] space-y-4">
      <div>
        <h2 className="text-[13px] font-semibold text-[var(--text-primary)]">
          {!havePackages
            ? "This container can’t serve a browser view yet"
            : skew
              ? "The installed browser isn’t the one Playwright launches"
              : noBrowser
                ? "Playwright is ready — but there’s no browser to drive yet"
                : "This container is set up"}
        </h2>
        <p className="mt-1 text-[13px] text-[var(--text-secondary)] leading-relaxed">
          {message ??
            (missing.length > 0
              ? `Missing: ${missing.join(", ")}.`
              : skew
                ? skewText(detection)
                : noBrowser
                  ? "Playwright and the viewer are installed. Install a browser below so there is something to watch."
                  : "Start the view from the button above once Claude has a browser open.")}
        </p>
      </div>

      <Step
        title="1. Playwright and the viewer UI"
        detail={
          <>
            Installs <Code>playwright</Code> and <Code>@playwright/cli</Code> into{" "}
            <Code>/workspace/node_modules</Code> inside the container. That directory is
            container storage — your project folders are mounted one level down, so
            nothing of yours is touched — and no <Code>sudo</Code> is involved. Small
            download; browsers come next.
          </>
        }
        done={havePackages}
        doneLabel={`Installed — playwright ${detection?.playwright_version ?? ""}, @playwright/cli ${detection?.cli_version ?? ""}`}
        action={
          <Button
            size="md"
            variant={havePackages ? "secondary" : "primary"}
            disabled={busy}
            onClick={() => onInstall("packages")}
          >
            {job === "packages" ? "Installing…" : havePackages ? "Reinstall" : "Set up Playwright"}
          </Button>
        }
      />

      <Step
        title="2. A browser to drive"
        detail={
          <>
            Both check the system libraries a browser links against first. Current base
            images ship them, so that step is normally skipped; a container built from an
            older image gets them installed with apt, which is the difference between a
            browser that downloads successfully and one that also starts. Both end by
            actually launching the browser to prove it works. Browsers land in{" "}
            <Code>~/.cache/ms-playwright</Code>, which is on the home volume, so they
            survive container recreation and are only lost on a project Reset.
          </>
        }
        done={browsers.length > 0 || chrome !== null}
        doneLabel={[
          browsers.length > 0 ? browsers.join(", ") : null,
          chrome ? `Chrome channel (${chrome})` : null,
        ]
          .filter(Boolean)
          .join(" · ")}
        action={
          <div className="flex flex-col gap-2 items-end">
            <Button
              size="md"
              variant={browsers.length > 0 || !havePackages ? "secondary" : "primary"}
              disabled={busy || !havePackages}
              onClick={() => onInstall("chromium")}
            >
              {job === "chromium" ? "Installing…" : "Install Chromium"}
            </Button>
            <Button
              size="md"
              disabled={busy || !havePackages}
              onClick={() => onInstall("chrome")}
            >
              {job === "chrome" ? "Installing…" : "Install Chrome channel"}
            </Button>
          </div>
        }
      >
        <ul className="mt-2 space-y-1 text-xs text-[var(--text-secondary)] leading-relaxed">
          <li>
            <strong className="text-[var(--text-primary)]">Chromium</strong> — Playwright’s
            own build, used by <Code>chromium.launch()</Code> with no channel. Several
            hundred MB.
          </li>
          <li>
            <strong className="text-[var(--text-primary)]">Chrome channel</strong> — Google
            Chrome from apt, which is what <Code>@playwright/mcp</Code> asks for. Install
            this one if Claude drives the browser through the MCP plugin. Roughly 150 MB.
          </li>
        </ul>
      </Step>

      {busy && (
        <p
          className="text-xs font-mono text-[var(--text-secondary)] break-all"
          aria-live="polite"
        >
          {progress ?? "Working…"}
        </p>
      )}

      {error && (
        <div className="text-xs text-[var(--error)]">
          <p className="font-semibold">That didn’t work.</p>
          <pre className="mt-1 whitespace-pre-wrap font-mono break-words text-[var(--text-secondary)]">
            {error}
          </pre>
        </div>
      )}

      {outcome?.warning && (
        <div className="text-xs text-[var(--text-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] p-3">
          <p className="font-semibold">Worth knowing</p>
          <p className="mt-1 whitespace-pre-wrap text-[var(--text-secondary)] leading-relaxed">
            {outcome.warning}
          </p>
        </div>
      )}

      {outcome?.log && (
        <AccordionSection
          id="browser-view-install-log"
          title="Install output"
          defaultOpen={false}
        >
          <pre className="p-3 text-xs font-mono whitespace-pre-wrap break-words text-[var(--text-secondary)] max-h-64 overflow-y-auto">
            {outcome.log}
          </pre>
        </AccordionSection>
      )}

      {detection && (
        <dl className="text-xs grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 pt-3 border-t border-[var(--border-color)]">
          <Detail label="Node.js" value={detection.node_version} />
          <Detail label="Playwright" value={detection.playwright_version} />
          <Detail label="Resolved from" value={detection.playwright_path} />
          <Detail
            label="browser.bind()"
            value={detection.has_bind ? "available" : "not in this build"}
          />
          <Detail label="@playwright/cli" value={detection.cli_version} />
          <Detail
            label="Browsers"
            value={browsers.length > 0 ? browsers.join(", ") : null}
          />
          <Detail label="Chrome channel" value={chrome} />
          {detection.searched.length > 0 && (
            <Detail label="Searched" value={detection.searched.join(", ")} />
          )}
        </dl>
      )}
    </div>
  );
}

/** One numbered setup step: what it does, whether it is done, and its button. */
function Step({
  title,
  detail,
  done,
  doneLabel,
  action,
  children,
}: {
  title: string;
  detail: React.ReactNode;
  done: boolean;
  doneLabel?: string;
  action: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <div className="border border-[var(--border-color)] rounded-[var(--radius-control)] p-3">
      <div className="flex items-start gap-3">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <h3 className="text-[13px] font-semibold text-[var(--text-primary)]">{title}</h3>
            <StatusIndicator tone={done ? "ok" : "off"} label={done ? "Installed" : "Not installed"} />
          </div>
          <p className="mt-1 text-xs text-[var(--text-secondary)] leading-relaxed">{detail}</p>
          {done && doneLabel && (
            <p className="mt-1 text-xs font-mono text-[var(--text-secondary)] break-all">
              {doneLabel}
            </p>
          )}
          {children}
        </div>
        <div className="flex-shrink-0">{action}</div>
      </div>
    </div>
  );
}

function Detail({ label, value }: { label: string; value: string | null }) {
  return (
    <>
      <dt className="text-[var(--text-secondary)]">{label}</dt>
      <dd className="font-mono text-[var(--text-primary)] break-all">
        {value ?? "not found"}
      </dd>
    </>
  );
}

function Explainer({
  title,
  tone = "normal",
  children,
}: {
  title: string;
  tone?: "normal" | "error";
  children: React.ReactNode;
}) {
  return (
    <div className="p-4 max-w-[46rem]">
      <h2
        className={`text-[13px] font-semibold ${
          tone === "error" ? "text-[var(--error)]" : "text-[var(--text-primary)]"
        }`}
      >
        {title}
      </h2>
      <p className="mt-1 text-[13px] text-[var(--text-secondary)] leading-relaxed">
        {children}
      </p>
    </div>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="font-mono text-xs px-1 py-0.5 rounded-[var(--radius-control)] bg-[var(--bg-tertiary)] text-[var(--text-primary)]">
      {children}
    </code>
  );
}
