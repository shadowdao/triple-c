/**
 * Client-side mirror of the scheduled-task rules in
 * `src-tauri/src/commands/inspect_commands.rs`, plus a plain-English reading of
 * a cron expression.
 *
 * The backend remains the authority — it re-validates everything and is the
 * only thing standing between a prompt and the container. This module exists so
 * the form can say what is wrong *before* a round trip, and so the cron field
 * can show the user what they actually typed.
 *
 * The cron rules match Debian/vixie cron, which is what the container runs:
 * five fields, names in month and day-of-week only, day-of-week 0–7, and a
 * `/step` only after `*` or a range (vixie rejects `1/2`).
 */

export const MAX_TASK_NAME_LEN = 100;
export const MAX_TASK_PROMPT_LEN = 8000;
export const MAX_WORKING_DIR_LEN = 512;
export const DEFAULT_WORKING_DIR = "/workspace";

const MAX_CRON_LEN = 256;
const MAX_CRON_STEP = 1000;

const MONTH_NAMES = [
  "jan", "feb", "mar", "apr", "may", "jun",
  "jul", "aug", "sep", "oct", "nov", "dec",
];
const DOW_NAMES = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

const MONTH_LABELS = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];
const DOW_LABELS = [
  "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

interface CronFieldSpec {
  label: string;
  min: number;
  max: number;
  names: string[];
  /** Numeric value of `names[0]` — 1 for January, 0 for Sunday. */
  nameBase: number;
}

const CRON_FIELDS: CronFieldSpec[] = [
  { label: "minute", min: 0, max: 59, names: [], nameBase: 0 },
  { label: "hour", min: 0, max: 23, names: [], nameBase: 0 },
  { label: "day of month", min: 1, max: 31, names: [], nameBase: 0 },
  { label: "month", min: 1, max: 12, names: MONTH_NAMES, nameBase: 1 },
  { label: "day of week", min: 0, max: 7, names: DOW_NAMES, nameBase: 0 },
];

/** A handful of schedules that cover most of what people actually want. */
export const CRON_PRESETS: { label: string; expression: string }[] = [
  { label: "Every 30 minutes", expression: "*/30 * * * *" },
  { label: "Hourly", expression: "0 * * * *" },
  { label: "Daily at 09:00", expression: "0 9 * * *" },
  { label: "Weekdays at 09:00", expression: "0 9 * * 1-5" },
  { label: "Mondays at 08:00", expression: "0 8 * * 1" },
];

// ── Field validation ─────────────────────────────────────────────────────────

/** `null` means valid; otherwise the message to show under the field. */
export type FieldError = string | null;

/** C0 and C1 control characters. */
// eslint-disable-next-line no-control-regex
const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F]/;
/** The same, minus tab / LF / CR — a multi-line prompt is normal. */
// eslint-disable-next-line no-control-regex
const CONTROL_CHARS_EXCEPT_WHITESPACE =
  /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F]/;

const hasControlChars = (value: string, allowNewlines: boolean) =>
  (allowNewlines ? CONTROL_CHARS_EXCEPT_WHITESPACE : CONTROL_CHARS).test(value);

export function validateTaskName(name: string): FieldError {
  const trimmed = name.trim();
  if (!trimmed) return "Task name is required.";
  if ([...trimmed].length > MAX_TASK_NAME_LEN)
    return `Task name is too long (max ${MAX_TASK_NAME_LEN} characters).`;
  if (hasControlChars(trimmed, false)) return "Task name must be a single line.";
  if (trimmed.startsWith("-")) return "Task name cannot start with “-”.";
  return null;
}

export function validateTaskPrompt(prompt: string): FieldError {
  const trimmed = prompt.trim();
  if (!trimmed) return "Task prompt is required.";
  if ([...trimmed].length > MAX_TASK_PROMPT_LEN)
    return `Task prompt is too long (max ${MAX_TASK_PROMPT_LEN} characters).`;
  if (hasControlChars(trimmed, true)) return "Task prompt contains an unsupported character.";
  return null;
}

export function validateWorkingDir(dir: string): FieldError {
  const trimmed = dir.trim();
  if (!trimmed) return null; // Blank falls back to /workspace, as the CLI does.
  if ([...trimmed].length > MAX_WORKING_DIR_LEN)
    return `Working directory is too long (max ${MAX_WORKING_DIR_LEN} characters).`;
  if (hasControlChars(trimmed, false)) return "Working directory must be a single line.";
  if (!trimmed.startsWith("/"))
    return "Working directory must be an absolute path inside the container, e.g. /workspace.";
  if (trimmed.split("/").includes("..")) return "Working directory cannot contain “..”.";
  return null;
}

// ── Cron ─────────────────────────────────────────────────────────────────────

function cronValue(spec: CronFieldSpec, token: string): number | null {
  if (token.length > 0 && /^[0-9]+$/.test(token)) {
    const value = Number(token);
    return value >= spec.min && value <= spec.max ? value : null;
  }
  const index = spec.names.indexOf(token.toLowerCase());
  return index >= 0 ? index + spec.nameBase : null;
}

function validateCronElement(spec: CronFieldSpec, element: string): FieldError {
  if (!element) return `Empty value in the ${spec.label} field.`;

  const slash = element.indexOf("/");
  const base = slash === -1 ? element : element.slice(0, slash);

  if (slash !== -1) {
    const raw = element.slice(slash + 1);
    if (!/^[0-9]{1,4}$/.test(raw))
      return `“${element}” in the ${spec.label} field: a step must be a number, like */5.`;
    const step = Number(raw);
    if (step < 1 || step > MAX_CRON_STEP)
      return `“${element}” in the ${spec.label} field: a step must be between 1 and ${MAX_CRON_STEP}.`;
    if (base !== "*" && !base.includes("-"))
      return `“${element}” in the ${spec.label} field: a step can only follow * or a range, like */5 or 1-5/2.`;
  }

  if (base === "*") return null;

  const dash = base.indexOf("-");
  const tokens = dash === -1 ? [base] : [base.slice(0, dash), base.slice(dash + 1)];
  for (const token of tokens) {
    if (cronValue(spec, token) === null) {
      return /^[0-9]+$/.test(token)
        ? `“${token}” is out of range for the ${spec.label} field (${spec.min}–${spec.max}).`
        : `“${token}” is not valid in the ${spec.label} field.`;
    }
  }
  return null;
}

export function validateCronExpression(expression: string): FieldError {
  if (expression.length > MAX_CRON_LEN)
    return `Cron expression is too long (max ${MAX_CRON_LEN} characters).`;
  const fields = expression.trim().split(/\s+/).filter(Boolean);
  if (fields.length !== 5)
    return `A cron schedule needs exactly 5 fields (minute hour day-of-month month day-of-week); got ${fields.length}.`;

  for (let i = 0; i < 5; i++) {
    for (const element of fields[i].split(",")) {
      const error = validateCronElement(CRON_FIELDS[i], element);
      if (error) return error;
    }
  }
  return null;
}

/** Matches the scheduler's own `--at` regex, then checks it is a real instant. */
export function validateAtTimestamp(at: string): FieldError {
  const trimmed = at.trim();
  if (!trimmed) return "A date and time is required.";
  const match = /^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2})$/.exec(trimmed);
  if (!match) return "Use the format YYYY-MM-DD HH:MM, e.g. 2026-12-25 09:05.";
  const [, y, mo, d, h, mi] = match.map(Number);
  const date = new Date(y, mo - 1, d, h, mi);
  const real =
    date.getFullYear() === y &&
    date.getMonth() === mo - 1 &&
    date.getDate() === d &&
    date.getHours() === h &&
    date.getMinutes() === mi;
  return real ? null : "That is not a real date and time.";
}

/** `true` when a valid one-shot time has already passed (a warning, not an error). */
export function atTimestampIsPast(at: string, now: Date = new Date()): boolean {
  if (validateAtTimestamp(at)) return false;
  const [datePart, timePart] = at.trim().split(" ");
  const [y, mo, d] = datePart.split("-").map(Number);
  const [h, mi] = timePart.split(":").map(Number);
  return new Date(y, mo - 1, d, h, mi).getTime() < now.getTime();
}

// ── Plain-English reading of a cron expression ───────────────────────────────

const pad = (n: number) => String(n).padStart(2, "0");

function joinList(items: string[]): string {
  if (items.length <= 1) return items[0] ?? "";
  if (items.length === 2) return `${items[0]} and ${items[1]}`;
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** The step of a bare `*​/n` field, or `null` for anything else. */
function simpleStep(field: string): number | null {
  const match = /^\*\/([0-9]+)$/.exec(field);
  return match ? Number(match[1]) : null;
}

/**
 * Every value a (already valid) field selects, or `null` for "all of them".
 * Bounded by the field's own range, so this cannot run away.
 */
function expandField(spec: CronFieldSpec, field: string): number[] | null {
  if (field === "*") return null;
  const values = new Set<number>();
  for (const element of field.split(",")) {
    const slash = element.indexOf("/");
    const base = slash === -1 ? element : element.slice(0, slash);
    const step = slash === -1 ? 1 : Number(element.slice(slash + 1));

    let from: number;
    let to: number;
    if (base === "*") {
      from = spec.min;
      to = spec.max;
    } else {
      const dash = base.indexOf("-");
      if (dash === -1) {
        from = to = cronValue(spec, base) as number;
      } else {
        from = cronValue(spec, base.slice(0, dash)) as number;
        to = cronValue(spec, base.slice(dash + 1)) as number;
      }
    }
    for (let v = from; v <= to; v += step) values.add(v);
  }
  const sorted = [...values].sort((a, b) => a - b);
  // A field that names every value reads better as "every".
  return sorted.length >= spec.max - spec.min + 1 ? null : sorted;
}

const isContiguous = (values: number[]) =>
  values.every((v, i) => i === 0 || v === values[i - 1] + 1);

function timePhrase(
  minutes: number[] | null,
  hours: number[] | null,
  minuteField: string,
  hourField: string,
): string {
  if (minutes === null && hours === null) return "Every minute";

  if (hours === null) {
    const step = simpleStep(minuteField);
    if (step !== null) return step === 1 ? "Every minute" : `Every ${step} minutes`;
    return `At ${joinList((minutes as number[]).map((m) => `:${pad(m)}`))} past every hour`;
  }

  if (minutes === null) {
    return `Every minute of ${joinList(hours.map((h) => `${pad(h)}:00`))}`;
  }

  const hourStep = simpleStep(hourField);
  if (hourStep !== null && minutes.length === 1) {
    return `At :${pad(minutes[0])} past every ${hourStep === 1 ? "hour" : `${hourStep} hours`}`;
  }
  if (minutes.length === 1 && hours.length >= 3 && isContiguous(hours)) {
    return `At :${pad(minutes[0])} past every hour from ${pad(hours[0])}:00 to ${pad(
      hours[hours.length - 1],
    )}:00`;
  }

  const times: string[] = [];
  for (const h of hours) for (const m of minutes) times.push(`${pad(h)}:${pad(m)}`);
  if (times.length <= 6) return `At ${joinList(times)}`;
  return `At minute ${joinList(minutes.map(String))} of hour ${joinList(hours.map(String))}`;
}

function weekdayPhrase(dows: number[]): string {
  const labels = dows.map((d) => DOW_LABELS[d]);
  if (dows.length >= 3 && isContiguous(dows))
    return `${labels[0]} to ${labels[labels.length - 1]}`;
  return joinList(labels);
}

function dayPhrase(doms: number[] | null, dows: number[] | null): string {
  if (doms === null && dows === null) return "every day";
  if (dows !== null && doms === null) return `on ${weekdayPhrase(dows)}`;
  if (doms !== null && dows === null)
    return `on day ${joinList(doms.map(String))} of the month`;
  // Cron ORs the two day fields when both are restricted.
  return `on day ${joinList((doms as number[]).map(String))} of the month or on ${weekdayPhrase(
    dows as number[],
  )}`;
}

/**
 * Read a cron expression back to the user in English, or `null` if it is not
 * valid. Deliberately a *reading*, not a scheduler: it never claims to know the
 * next run time.
 */
export function describeCron(expression: string): string | null {
  if (validateCronExpression(expression)) return null;
  const [minuteField, hourField, domField, monthField, dowField] = expression
    .trim()
    .split(/\s+/);

  const minutes = expandField(CRON_FIELDS[0], minuteField);
  const hours = expandField(CRON_FIELDS[1], hourField);
  const doms = expandField(CRON_FIELDS[2], domField);
  const months = expandField(CRON_FIELDS[3], monthField);

  let dows = expandField(CRON_FIELDS[4], dowField);
  if (dows) {
    // 0 and 7 are both Sunday.
    dows = [...new Set(dows.map((d) => (d === 7 ? 0 : d)))].sort((a, b) => a - b);
    if (dows.length === 7) dows = null;
  }

  const monthPart =
    months === null ? "" : ` in ${joinList(months.map((m) => MONTH_LABELS[m - 1]))}`;

  return `${timePhrase(minutes, hours, minuteField, hourField)}, ${dayPhrase(
    doms,
    dows,
  )}${monthPart}.`;
}
