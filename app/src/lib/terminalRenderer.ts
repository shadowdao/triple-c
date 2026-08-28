/**
 * Decides whether the terminal loads `@xterm/addon-webgl`.
 *
 * Split out of `TerminalView` so it can be unit-tested without standing up a
 * terminal, and so the platform rule lives in exactly one place.
 */

/** True when the webview is running on Linux (WebKitGTK), excluding Android. */
export function isLinuxWebview(userAgent: string): boolean {
  return /\bLinux\b/.test(userAgent) && !/\bAndroid\b/.test(userAgent);
}

/**
 * Resolve the effective WebGL setting.
 *
 * `setting` is `AppSettings.terminal_gpu_rendering`: `true`/`false` force the
 * answer, `null`/`undefined` mean auto. Auto is on everywhere except Linux —
 * there the app disables WebKitGTK's DMA-BUF renderer at startup (triple-c#34),
 * which leaves WebGL present but software-rasterised, so loading the addon is
 * slower than the canvas renderer it would otherwise have fallen back to.
 */
export function resolveTerminalGpuRendering(
  setting: boolean | null | undefined,
  userAgent: string,
): boolean {
  if (typeof setting === "boolean") return setting;
  return !isLinuxWebview(userAgent);
}
