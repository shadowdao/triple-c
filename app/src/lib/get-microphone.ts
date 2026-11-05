/**
 * Open the microphone, relaxing constraints until the webview accepts them.
 *
 * Attempts, in order:
 *   1. saved device (exact) + base constraints
 *   2. base constraints, system default device
 *   3. bare `audio: true`
 *
 * Step 2 covers a stale saved deviceId (unplugged / regenerated IDs). Step 3
 * covers webviews (notably WebKitGTK on Linux) that reject one of the optional
 * processing constraints with `OverconstrainedError: Invalid constraint`.
 * Only constraint/device errors fall through; permission errors still throw.
 */
const RETRYABLE = new Set(["OverconstrainedError", "NotFoundError"]);

export async function getMicrophoneStream(
  base: MediaTrackConstraints,
  deviceId?: string | null,
): Promise<MediaStream> {
  const attempts: (MediaTrackConstraints | boolean)[] = [];
  if (deviceId) attempts.push({ ...base, deviceId: { exact: deviceId } });
  attempts.push(base, true);

  let lastError: unknown;
  for (const audio of attempts) {
    try {
      return await navigator.mediaDevices.getUserMedia({ audio });
    } catch (e) {
      lastError = e;
      if (!(e instanceof DOMException) || !RETRYABLE.has(e.name)) throw e;
      console.warn("getUserMedia attempt failed, relaxing constraints:", e.name, e.message, audio);
    }
  }
  throw lastError;
}
