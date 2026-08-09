import { describe, it, expect } from "vitest";
import {
  atTimestampIsPast,
  describeCron,
  validateAtTimestamp,
  validateCronExpression,
  validateTaskName,
  validateTaskPrompt,
  validateWorkingDir,
  MAX_TASK_NAME_LEN,
  MAX_TASK_PROMPT_LEN,
} from "./taskValidation";

describe("task field validation", () => {
  it("requires a name that cannot be read as an option", () => {
    expect(validateTaskName("nightly")).toBeNull();
    expect(validateTaskName("  nightly  ")).toBeNull();
    expect(validateTaskName("")).toMatch(/required/i);
    expect(validateTaskName("   ")).toMatch(/required/i);
    expect(validateTaskName("-id")).toMatch(/cannot start/i);
    expect(validateTaskName("--prompt")).toMatch(/cannot start/i);
    expect(validateTaskName("two\nlines")).toMatch(/single line/i);
    expect(validateTaskName("n".repeat(MAX_TASK_NAME_LEN + 1))).toMatch(/too long/i);
  });

  it("treats shell syntax in a name or prompt as ordinary text", () => {
    // Nothing downstream is a shell, so these must not be rejected —
    // over-blocking would be its own bug.
    for (const value of ["; rm -rf /", "$(id)", "`id`", "a | b && c", "%pct"]) {
      expect(validateTaskName(value)).toBeNull();
      expect(validateTaskPrompt(value)).toBeNull();
    }
  });

  it("requires a prompt and allows it to be multi-line", () => {
    expect(validateTaskPrompt("Run the tests\nthen report")).toBeNull();
    expect(validateTaskPrompt("")).toMatch(/required/i);
    expect(validateTaskPrompt("  \n  ")).toMatch(/required/i);
    expect(validateTaskPrompt("p".repeat(MAX_TASK_PROMPT_LEN + 1))).toMatch(/too long/i);
    expect(validateTaskPrompt("bad\u0000nul")).toMatch(/unsupported/i);
  });

  it("requires an absolute working directory, defaulting when blank", () => {
    expect(validateWorkingDir("")).toBeNull();
    expect(validateWorkingDir("/workspace/app")).toBeNull();
    expect(validateWorkingDir("workspace")).toMatch(/absolute/i);
    expect(validateWorkingDir("./rel")).toMatch(/absolute/i);
    expect(validateWorkingDir("~/home")).toMatch(/absolute/i);
    expect(validateWorkingDir("/workspace/../etc")).toMatch(/\.\./);
  });
});

describe("cron validation", () => {
  // Every expression below was checked against the container's own
  // Debian/vixie `crontab` binary, which is the thing that ultimately accepts
  // or rejects the schedule.
  it("accepts expressions vixie cron accepts", () => {
    for (const good of [
      "* * * * *",
      "*/30 * * * *",
      "0 3 * * *",
      "0 9 * * 1-5",
      "0,30 9-17 * * 1-5",
      "15 0 1 1 *",
      "0 9 * * 0",
      "0 9 * * 7",
      "0 9 * * MON-FRI",
      "0 0 1 JAN *",
      "0-59/70 * * * *",
      "1-5/2 * * * *",
      "05 09 * * *",
    ]) {
      expect(validateCronExpression(good), good).toBeNull();
    }
  });

  it("rejects expressions vixie cron rejects", () => {
    for (const bad of [
      "",
      "* * * *",
      "* * * * * *",
      "@daily",
      "not a cron",
      "99 * * * *",
      "0 24 * * *",
      "0 0 0 1 *",
      "0 9 * * 8",
      "0 9 * 13 *",
      "*/0 * * * *",
      "1/2 * * * *",
      "0 9 * * jan",
      "jan 9 * * *",
      "0 9 * * mon,",
      "0 9 * * 1--5",
      "0 9 * * 1-5/x",
      "0 9 * * *; rm -rf /",
      "$(id) * * * *",
    ]) {
      expect(validateCronExpression(bad), bad).not.toBeNull();
    }
  });

  it("matches the backend's message shape for the field count", () => {
    expect(validateCronExpression("* * * *")).toMatch(/exactly 5 fields/);
  });
});

describe("describeCron", () => {
  const cases: [string, string][] = [
    ["* * * * *", "Every minute, every day."],
    ["*/30 * * * *", "Every 30 minutes, every day."],
    ["0 * * * *", "At :00 past every hour, every day."],
    ["0,30 * * * *", "At :00 and :30 past every hour, every day."],
    ["0 9 * * *", "At 09:00, every day."],
    ["30 9 * * 1-5", "At 09:30, on Monday to Friday."],
    ["0 8 * * 1", "At 08:00, on Monday."],
    ["0 9 * * 0", "At 09:00, on Sunday."],
    // 7 is Sunday too, and must not read as an eighth day.
    ["0 9 * * 7", "At 09:00, on Sunday."],
    ["0 9,17 * * *", "At 09:00 and 17:00, every day."],
    ["0 9-17 * * *", "At :00 past every hour from 09:00 to 17:00, every day."],
    ["0 */2 * * *", "At :00 past every 2 hours, every day."],
    ["0 0 1 * *", "At 00:00, on day 1 of the month."],
    ["0 0 1 1 *", "At 00:00, on day 1 of the month in January."],
    ["0 9 * * MON,THU", "At 09:00, on Monday and Thursday."],
  ];

  it.each(cases)("reads %s as %s", (expression, expected) => {
    expect(describeCron(expression)).toBe(expected);
  });

  it("says nothing rather than guessing when the expression is invalid", () => {
    expect(describeCron("nope")).toBeNull();
    expect(describeCron("99 * * * *")).toBeNull();
  });
});

describe("one-shot timestamps", () => {
  it("accepts only the scheduler's own format", () => {
    expect(validateAtTimestamp("2026-12-25 09:05")).toBeNull();
    expect(validateAtTimestamp("")).toMatch(/required/i);
    // The scheduler's regex demands two digits everywhere.
    expect(validateAtTimestamp("2026-1-5 09:05")).toMatch(/YYYY-MM-DD/);
    expect(validateAtTimestamp("2026-12-25T09:05")).toMatch(/YYYY-MM-DD/);
    expect(validateAtTimestamp("2026-12-25 09:05:00")).toMatch(/YYYY-MM-DD/);
    expect(validateAtTimestamp("2026-02-30 09:05")).toMatch(/not a real/i);
    expect(validateAtTimestamp("2026-12-25 25:00")).toMatch(/not a real/i);
  });

  it("flags a time in the past, because cron would fire it next year", () => {
    const now = new Date(2026, 5, 1, 12, 0);
    expect(atTimestampIsPast("2026-05-31 09:00", now)).toBe(true);
    expect(atTimestampIsPast("2026-06-01 12:01", now)).toBe(false);
    expect(atTimestampIsPast("nonsense", now)).toBe(false);
  });
});
