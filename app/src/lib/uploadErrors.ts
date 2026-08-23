/**
 * The one place the frontend agrees with Rust about "that name is taken".
 *
 * `upload_file_to_container` used to clobber whatever was already at the
 * destination, which is the wrong default for a drop: a drag is aimed with a
 * mouse, and the file it lands on is frequently not the file the user meant to
 * replace. So the backend refuses by default and the frontend asks — but only
 * if it can tell *this* refusal apart from "permission denied" or "no space
 * left", because an overwrite prompt raised over an unrelated failure would
 * offer a button that cannot possibly work.
 *
 * **This module is the contract point, and the Rust half has to hold up its
 * end**: `upload_file_to_container` must put `FILE_EXISTS_MARKER` in the error
 * it returns when the destination already exists, ideally in the agreed shape
 *
 *     FILE_EXISTS: /workspace/notes.txt already exists
 *
 * and must accept an `overwrite: bool` argument that skips the check. Nothing
 * here parses a human sentence — the marker is the whole agreement, and the
 * path is a bonus that is only used to name the file in the prompt.
 *
 * The predicate is deliberately tolerant about the *shape* of the error rather
 * than its wording, because a Tauri command error crosses the IPC boundary as
 * whatever `serde` made of it: a bare string from `Err(String)`, an object from
 * a `#[derive(Serialize)]` error enum, or an `Error` if a JS layer wrapped it
 * on the way through. All three are the same refusal, and the UI must not
 * behave differently depending on which one a future refactor produces.
 *
 * **Tolerant about shape is not the same as tolerant about content.** This
 * used to normalise the whole error (lower-case, `_`/`-` stripped) and ask
 * whether `fileexists` appeared *anywhere* in it — which a host file named
 * `file-exists.txt` satisfies on its way through any error at all. Uploading
 * that file and hitting "permission denied" therefore raised the overwrite
 * prompt, and answering Replace re-invoked the upload with `overwrite: true`:
 * an unrelated failure silently promoted into an overwrite of whatever shared
 * the name in the container. So the marker now has to appear in a form a
 * *filename* cannot produce:
 *
 * - in prose, the canonical `FILE_EXISTS` (or `FILE-EXISTS`) in upper case,
 *   standing alone — end of string, or followed by the `:`/`=` of the agreed
 *   `FILE_EXISTS: <path>` form. `file-exists.txt`, `FILE_EXISTS.txt` and
 *   `/workspace/FILE_EXISTS` all fail that, because a filename brings its own
 *   extension, quote or path separator along with it.
 * - in a discriminant field, the *whole* value, case- and separator-insensitive
 *   (`FileExists`, `file_exists`, `file-exists`, `FileExistsError`) — a
 *   discriminant is a variant name, not a sentence, so equality is the right
 *   test and a filename never gets to be one.
 */

/** Marker the backend puts in the error for "a file with this name is already there". */
export const FILE_EXISTS_MARKER = "FILE_EXISTS";

/**
 * Structured error shapes carry the marker in a discriminant rather than in
 * prose. These are the field names a serialised Rust error realistically uses;
 * matching is case-insensitive and ignores `_`/`-` so `FileExists`,
 * `file_exists` and `FILE-EXISTS` all read as the same variant.
 */
const KIND_FIELDS = ["kind", "code", "type", "error", "reason"] as const;
const MESSAGE_FIELDS = ["message", "msg", "detail", "description"] as const;
const PATH_FIELDS = ["path", "container_path", "containerPath", "target", "file"] as const;

/** `FileExists` / `file-exists` / `FILE_EXISTS` all normalise to `fileexists`. */
function normaliseKind(value: string): string {
  return value.toLowerCase().replace(/[\s_-]/g, "");
}

const KIND_NEEDLE = normaliseKind(FILE_EXISTS_MARKER);

/**
 * The marker standing on its own inside a sentence.
 *
 * Derived from `FILE_EXISTS_MARKER` so the two cannot drift. Upper case is
 * load-bearing (a lower-case `file-exists` is a plausible filename, the
 * upper-case token is not), and so is the lookahead: the marker must end the
 * string or be followed by the `:`/`=` that introduces the path. That is what
 * a path or a filename cannot forge — `FILE_EXISTS.txt`, `"FILE_EXISTS"` and
 * `/workspace/FILE_EXISTS` are each rejected by one end or the other.
 */
const PROSE_MARKER = new RegExp(
  `(?:^|[\\s:;(\\[{"'\`])${FILE_EXISTS_MARKER.replace(/_/g, "[_-]")}(?=$|[\\s:=])`,
);

/** A discriminant *is* the refusal, rather than mentioning it. */
function isFileExistsDiscriminant(value: string): boolean {
  const normalised = normaliseKind(value);
  return normalised === KIND_NEEDLE || normalised === `${KIND_NEEDLE}error`;
}

function asRecord(e: unknown): Record<string, unknown> | null {
  return typeof e === "object" && e !== null ? (e as Record<string, unknown>) : null;
}

/**
 * Every string an error carries, flattened: the error itself if it is one, its
 * message-ish fields, and its kind-ish fields. Nesting is followed one level
 * because a wrapped error (`{ error: { kind: … } }`) is the same refusal.
 */
function stringsIn(e: unknown, depth = 0): string[] {
  const { prose, kinds } = partitionStrings(e, depth);
  return [...prose, ...kinds];
}

/**
 * The same flattening, but keeping track of *where* each string came from.
 *
 * A discriminant field and a message field are held to different standards
 * (see the module comment), so they cannot be pooled. `error` is listed as a
 * discriminant field and yet routinely carries a whole sentence, which is why
 * a kind string is tested against both rules and a prose string only against
 * the prose one.
 */
function partitionStrings(
  e: unknown,
  depth = 0,
): { prose: string[]; kinds: string[] } {
  if (typeof e === "string") return { prose: [e], kinds: [] };
  if (e instanceof Error) return { prose: [e.message], kinds: [e.name] };
  const record = asRecord(e);
  if (!record || depth > 1) return { prose: [], kinds: [] };
  const prose: string[] = [];
  const kinds: string[] = [];
  const walk = (value: unknown, into: string[]) => {
    if (typeof value === "string") into.push(value);
    else if (value !== undefined) {
      const nested = partitionStrings(value, depth + 1);
      prose.push(...nested.prose);
      kinds.push(...nested.kinds);
    }
  };
  for (const field of KIND_FIELDS) walk(record[field], kinds);
  for (const field of MESSAGE_FIELDS) walk(record[field], prose);
  return { prose, kinds };
}

/**
 * True when the backend refused an upload because the destination is taken.
 *
 * Accepts a bare string, an `Error`, or an object with a `kind`/`code`
 * discriminant or a `message` — see the module comment for why all three have
 * to work.
 */
export function isFileExistsError(e: unknown): boolean {
  const { prose, kinds } = partitionStrings(e);
  return (
    kinds.some((s) => isFileExistsDiscriminant(s) || PROSE_MARKER.test(s)) ||
    prose.some((s) => PROSE_MARKER.test(s))
  );
}

/**
 * The container path the conflict is about, when the error carries one — used
 * only to name the file in the prompt, so `null` is a perfectly good answer
 * and the caller falls back to the host path it was uploading.
 */
export function fileExistsPath(e: unknown): string | null {
  const record = asRecord(e);
  if (record) {
    for (const field of PATH_FIELDS) {
      const value = record[field];
      if (typeof value === "string" && value.length > 0) return value;
    }
    // One level down, for `{ error: { path } }`.
    for (const field of KIND_FIELDS) {
      const nested = fileExistsPath(record[field]);
      if (nested) return nested;
    }
  }
  for (const s of stringsIn(e)) {
    // The agreed prose form: `FILE_EXISTS: <path>` — everything up to the
    // first space after the marker.
    const match = new RegExp(`${FILE_EXISTS_MARKER}\\s*[:=]\\s*(\\S+)`).exec(s);
    if (match) return match[1];
  }
  return null;
}

/**
 * What the user answered to one conflict. The blanket answers exist because a
 * ten-file drop onto a populated directory is ten prompts otherwise, which is
 * the kind of dialog people dismiss without reading.
 */
export type OverwriteChoice = "replace" | "skip" | "replace-all" | "skip-all";

/**
 * Fragments that identify a refusal the backend already wrote **for a person**.
 *
 * The file commands guard two policies that a user can trip over by accident,
 * and both answer with a finished sentence that names the offending path and
 * says what to do instead:
 *
 *     ".ssh" is a hidden folder — Triple-C will not save there. Choose a visible location.
 *     Folder path is outside the folders this panel can change (/workspace, /home/claude, /tmp): /etc
 *
 * Those sentences were being used as the *detail* of a generic toast
 * ("A file could not be uploaded"), and `ToastHost` renders a detail as
 * collapsed monospace behind a "Details" button — so the one part of the
 * message that explained anything was the part nobody saw. Matching them here
 * lets the caller promote the sentence to the toast's headline.
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
