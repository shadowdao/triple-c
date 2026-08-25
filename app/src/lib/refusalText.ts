/**
 * Turning a backend refusal into the sentence a person reads.
 *
 * Tauri command errors cross the IPC boundary as whatever `serde` made of them:
 * a bare string from `Err(String)`, an object from a `#[derive(Serialize)]`
 * error enum, or an `Error` if a JS layer wrapped it on the way through. All
 * three are the same refusal, and the UI must not read differently depending on
 * which one a future refactor produces — so everything here is tolerant about
 * the *shape* of an error and picks the most human string out of it.
 */

/**
 * Field names a serialised Rust error realistically uses for its discriminant
 * and for its human text. `error` is listed as a discriminant field and yet
 * routinely carries a whole sentence, which is why a kind string is read as
 * prose too.
 */
const KIND_FIELDS = ["kind", "code", "type", "error", "reason"] as const;
const MESSAGE_FIELDS = ["message", "msg", "detail", "description"] as const;

function asRecord(e: unknown): Record<string, unknown> | null {
  return typeof e === "object" && e !== null ? (e as Record<string, unknown>) : null;
}

/**
 * Every string an error carries, flattened: the error itself if it is one, its
 * message-ish fields, and its kind-ish fields. Nesting is followed one level
 * because a wrapped error (`{ error: { message: … } }`) is the same refusal.
 */
function stringsIn(e: unknown, depth = 0): string[] {
  if (typeof e === "string") return [e];
  if (e instanceof Error) return [e.message, e.name];
  const record = asRecord(e);
  if (!record || depth > 1) return [];
  const out: string[] = [];
  const walk = (value: unknown) => {
    if (typeof value === "string") out.push(value);
    else if (value !== undefined) out.push(...stringsIn(value, depth + 1));
  };
  for (const field of KIND_FIELDS) walk(record[field]);
  for (const field of MESSAGE_FIELDS) walk(record[field]);
  return out;
}

/**
 * Fragments that identify a refusal the backend already wrote **for a person**.
 *
 * The file commands guard two policies that a user can trip over by accident,
 * and both answer with a finished sentence that names the offending path and
 * says what to do instead:
 *
 *     the path goes through ".ssh", a hidden folder — Triple-C will not save …
 *     Folder path is outside the folders this panel can change (/workspace, /home/claude, /tmp): /etc
 *
 * Those sentences were being used as the *detail* of a generic toast, and
 * `ToastHost` renders a detail as collapsed monospace behind a "Details"
 * button — so the one part of the message that explained anything was the part
 * nobody saw. Matching them here lets the caller promote the sentence to the
 * toast's headline.
 *
 * Matched on a stable fragment rather than the whole string, because the path
 * and the verb ("save"/"read", "file"/"folder") vary per call. Deliberately a
 * short list: an error that is *not* recognised still reports exactly as it
 * did before, so a wrong guess here can only fail to promote, never mangle.
 */
const REFUSAL_MARKERS = [
  // `validate_host_path` — hidden host component, and system locations.
  "Triple-C will not",
  // `validate_container_write_path` — outside /workspace, /home/claude, /tmp.
  "outside the folders this panel can change",
] as const;

/**
 * `Error: …`, `TypeError: …`, `invoke failed: …` — wrappers a JS layer may have
 * put in front of the backend's sentence on the way through. Stripped so the
 * prose starts where the backend started it; applied twice at most, because a
 * doubly-wrapped error is the realistic worst case and looping on user text is
 * not.
 */
const WRAPPER_PREFIX = /^(?:uncaught\s*(?:\(in promise\)\s*)?)?(?:[a-z]*error|invoke(?:\s+failed)?)\s*:\s*/i;

function stripWrapper(text: string): string {
  let out = text.trim();
  for (let i = 0; i < 2; i++) {
    const next = out.replace(WRAPPER_PREFIX, "").trim();
    if (next === out) break;
    out = next;
  }
  return out;
}

/**
 * The backend's own user-facing sentence, when this failure is one — otherwise
 * `null`, and the caller reports it however it reported everything else.
 */
export function readableRefusal(e: unknown): string | null {
  for (const s of stringsIn(e)) {
    const text = stripWrapper(s);
    if (REFUSAL_MARKERS.some((marker) => text.includes(marker))) return text;
  }
  return null;
}

/**
 * The most human form of any failure, for the places that show one verbatim.
 *
 * `String(e)` is what these used to be, which turns a serialised error object
 * into `[object Object]` and leaves a JS wrapper prefix on a sentence that
 * reads perfectly well without it.
 */
export function errorText(e: unknown): string {
  const readable = readableRefusal(e);
  if (readable) return readable;
  if (typeof e === "string") return stripWrapper(e);
  if (e instanceof Error) return stripWrapper(e.message);
  const record = asRecord(e);
  if (record) {
    for (const field of [...MESSAGE_FIELDS, ...KIND_FIELDS]) {
      const value = record[field];
      if (typeof value === "string" && value.trim().length > 0) return stripWrapper(value);
    }
  }
  return String(e);
}
