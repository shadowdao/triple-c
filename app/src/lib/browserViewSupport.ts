/**
 * What a container has to have before anything can be opened *inside* it.
 *
 * The Browser tab asks this to decide what to offer; the terminal's URL toast
 * asks it to decide which of its two buttons should lead. Both need the same
 * answer, so the predicates live here rather than beside either caller — the
 * failure this avoids is the toast steering a user at a container-side browser
 * that the Browser tab is, on the very same screen, offering to install.
 *
 * The important thing to know about `PlaywrightDetection` is that browsers are
 * deliberately **not** baked into the image: the libraries they link against
 * are, the binaries are a user-pressed install. So "Playwright is present" and
 * "a page can actually be opened" are two different questions, and a fresh
 * project answers yes to neither.
 */

import type { PlaywrightDetection } from "./types";

/**
 * Mirrors Rust `PlaywrightDetection::is_usable` — the packages the live
 * dashboard needs. Says nothing about whether a browser exists to show in it.
 */
export function isBrowserViewUsable(d: PlaywrightDetection | null): boolean {
  return d !== null && d.playwright_version !== null && d.has_bind && d.cli_entry !== null;
}

/**
 * Whether `openPageInContainerBrowser` has a browser to launch.
 *
 * Stricter than {@link isBrowserViewUsable} on purpose: the packages can be
 * installed with `~/.cache/ms-playwright` still empty, which is exactly the
 * state a `playwright install` step exists to leave behind, and launching into
 * it fails several seconds after the click.
 *
 * Unknown reads as "no". A probe that could not run (stopped container, an
 * image predating these fields) leaves the executable fields absent, and the
 * caller's fallback — the host browser — is the one that at least reports its
 * own failure. Over-refusing costs a user one extra click on a button that is
 * still right there; over-accepting costs them a sign-in that goes nowhere.
 */
export function canOpenPageInContainerBrowser(d: PlaywrightDetection | null): boolean {
  if (!isBrowserViewUsable(d) || !d) return false;
  // The viewer's own Chromium, confirmed on disk by the probe.
  if (d.chromium_executable_exists) return true;
  // Google Chrome is an apt package, so it is never in `browsers` and has no
  // revision to skew against.
  if (d.chrome_channel !== null) return true;
  // `== null`, not `=== null`: a probe from a container predating the
  // executable fields omits them entirely, and `undefined` there means "didn't
  // answer", not "missing". In that case a non-empty bundle list is the only
  // evidence available, and it is better than nothing.
  return d.chromium_executable == null && d.browsers.length > 0;
}
