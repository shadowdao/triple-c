import { useId, useMemo, useRef, useState } from "react";
import type { Project, ScheduledTask, ScheduledTaskInput, ScheduleKind } from "../../../lib/types";
import { addScheduledTask, updateScheduledTask } from "../../../lib/tauri-commands";
import { effectivePermissionMode, PERMISSION_MODES } from "../PermissionModeControl";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";
import SegmentedControl from "../../ui/SegmentedControl";
import { inputClass, monoInputClass } from "../../ui/Field";
import {
  atTimestampIsPast,
  CRON_PRESETS,
  DEFAULT_WORKING_DIR,
  describeCron,
  MAX_TASK_PROMPT_LEN,
  validateAtTimestamp,
  validateCronExpression,
  validateTaskName,
  validateTaskPrompt,
  validateWorkingDir,
} from "./taskValidation";

interface Props {
  project: Project;
  /** `null` creates a new task; a task edits it in place. */
  task: ScheduledTask | null;
  onClose: () => void;
  /** Called after the scheduler accepted the change, to refresh the list. */
  onSaved: () => void;
}

const DEFAULT_CRON = "0 9 * * *";

/** `YYYY-MM-DD HH:MM`, one hour from now, as the one-shot default. */
function defaultAtTimestamp(now = new Date()): string {
  const at = new Date(now.getTime() + 60 * 60 * 1000);
  at.setSeconds(0, 0);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(
    at.getHours(),
  )}:${pad(at.getMinutes())}`;
}

/**
 * Create or edit a `triple-c-scheduler` task.
 *
 * Validation here mirrors the backend so mistakes surface before a round trip;
 * the backend re-checks everything regardless.
 */
export default function TaskEditorModal({ project, task, onClose, onSaved }: Props) {
  const formId = useId();
  const nameRef = useRef<HTMLInputElement>(null);

  const [name, setName] = useState(task?.name ?? "");
  const [prompt, setPrompt] = useState(task?.prompt ?? "");
  const [workingDir, setWorkingDir] = useState(task?.working_dir ?? DEFAULT_WORKING_DIR);
  const [kind, setKind] = useState<ScheduleKind>(
    task?.task_type === "once" ? "once" : "recurring",
  );
  const [cron, setCron] = useState(
    task && task.task_type !== "once" ? task.schedule : DEFAULT_CRON,
  );
  const [at, setAt] = useState(task?.at ?? defaultAtTimestamp());

  const [showAllErrors, setShowAllErrors] = useState(false);
  const [touched, setTouched] = useState<Record<string, boolean>>({});
  const [saving, setSaving] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const errors = {
    name: validateTaskName(name),
    prompt: validateTaskPrompt(prompt),
    workingDir: validateWorkingDir(workingDir),
    schedule: kind === "recurring" ? validateCronExpression(cron) : validateAtTimestamp(at),
  };
  const hasErrors = Object.values(errors).some(Boolean);

  const show = (field: keyof typeof errors) =>
    (showAllErrors || touched[field]) && errors[field] ? errors[field] : null;

  const cronReading = useMemo(() => describeCron(cron), [cron]);
  const atIsPast = kind === "once" && atTimestampIsPast(at);

  const mode = effectivePermissionMode(project);
  const modeLabel = PERMISSION_MODES.find((m) => m.value === mode)?.label ?? mode;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setShowAllErrors(true);
    setSubmitError(null);
    if (hasErrors) return;

    const input: ScheduledTaskInput = {
      name: name.trim(),
      prompt: prompt.trim(),
      scheduleKind: kind,
      schedule: kind === "recurring" ? cron.trim() : at.trim(),
      workingDir: workingDir.trim() || DEFAULT_WORKING_DIR,
    };

    setSaving(true);
    try {
      if (task) {
        await updateScheduledTask(project.id, task.id, input, task.enabled);
      } else {
        await addScheduledTask(project.id, input);
      }
      onSaved();
      onClose();
    } catch (err) {
      setSubmitError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const errorText = (message: string | null) =>
    message ? (
      <p role="alert" className="mt-1 text-xs text-[var(--error)]">
        {message}
      </p>
    ) : null;

  return (
    <Modal
      title={task ? `Edit task — ${task.name}` : "New scheduled task"}
      onClose={onClose}
      widthClassName="w-[40rem]"
      initialFocusRef={nameRef}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={saving}>
            Cancel
          </Button>
          <Button size="md" variant="primary" type="submit" form={formId} disabled={saving}>
            {saving ? "Saving…" : task ? "Save changes" : "Create task"}
          </Button>
        </>
      }
    >
      <form id={formId} onSubmit={handleSubmit} className="space-y-4">
        {/* Name */}
        <div>
          <label
            htmlFor={`${formId}-name`}
            className="block text-[13px] font-medium text-[var(--text-primary)] mb-1"
          >
            Name
          </label>
          <input
            id={`${formId}-name`}
            ref={nameRef}
            value={name}
            onChange={(e) => setName(e.target.value)}
            onBlur={() => setTouched((t) => ({ ...t, name: true }))}
            placeholder="nightly-tests"
            aria-invalid={show("name") ? true : undefined}
            className={inputClass}
          />
          {errorText(show("name"))}
        </div>

        {/* Prompt */}
        <div>
          <label
            htmlFor={`${formId}-prompt`}
            className="block text-[13px] font-medium text-[var(--text-primary)]"
          >
            Prompt
          </label>
          <p className="mt-0.5 mb-1 text-xs text-[var(--text-secondary)] leading-snug">
            What Claude Code is asked to do on each run.
          </p>
          <textarea
            id={`${formId}-prompt`}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            onBlur={() => setTouched((t) => ({ ...t, prompt: true }))}
            rows={4}
            maxLength={MAX_TASK_PROMPT_LEN}
            placeholder="Run the test suite and summarise any failures."
            aria-invalid={show("prompt") ? true : undefined}
            className={`${inputClass} resize-y`}
          />
          {errorText(show("prompt"))}
        </div>

        {/* Schedule */}
        <div>
          <span className="block text-[13px] font-medium text-[var(--text-primary)] mb-1">
            Schedule
          </span>
          <SegmentedControl
            label="Schedule kind"
            segments={[
              { value: "recurring", label: "Recurring" },
              { value: "once", label: "Once" },
            ]}
            value={kind}
            onChange={(v) => {
              setKind(v);
              setSubmitError(null);
            }}
          />

          {kind === "recurring" ? (
            <div className="mt-2 space-y-2">
              <div className="flex flex-wrap gap-1">
                {CRON_PRESETS.map((preset) => (
                  <Button
                    key={preset.expression}
                    onClick={() => {
                      setCron(preset.expression);
                      setTouched((t) => ({ ...t, schedule: true }));
                    }}
                  >
                    {preset.label}
                  </Button>
                ))}
              </div>
              <input
                id={`${formId}-cron`}
                value={cron}
                onChange={(e) => setCron(e.target.value)}
                onBlur={() => setTouched((t) => ({ ...t, schedule: true }))}
                placeholder="0 9 * * 1-5"
                aria-label="Cron expression"
                aria-describedby={`${formId}-cron-reading`}
                aria-invalid={show("schedule") ? true : undefined}
                className={monoInputClass}
              />
              <p
                id={`${formId}-cron-reading`}
                aria-live="polite"
                className="text-xs text-[var(--text-secondary)]"
              >
                <span className="font-mono">minute hour day-of-month month day-of-week</span> ·{" "}
                {cronReading ? (
                  <span className="text-[var(--text-primary)]">{cronReading}</span>
                ) : (
                  <span>not a valid schedule yet</span>
                )}
              </p>
              {errorText(show("schedule"))}
            </div>
          ) : (
            <div className="mt-2 space-y-1">
              <input
                id={`${formId}-at`}
                value={at}
                onChange={(e) => setAt(e.target.value)}
                onBlur={() => setTouched((t) => ({ ...t, schedule: true }))}
                placeholder="2026-12-25 09:05"
                aria-label="Run at (YYYY-MM-DD HH:MM)"
                aria-invalid={show("schedule") ? true : undefined}
                className={monoInputClass}
              />
              <p className="text-xs text-[var(--text-secondary)]">
                Container local time, as <code className="font-mono">YYYY-MM-DD HH:MM</code>. The
                task removes itself after it runs.
              </p>
              {atIsPast && (
                <p className="text-xs text-[var(--warning)]">
                  That time has already passed. A one-shot task is stored as a cron entry without a
                  year, so it would next fire on that date next year.
                </p>
              )}
              {errorText(show("schedule"))}
            </div>
          )}
        </div>

        {/* Working directory */}
        <div>
          <label
            htmlFor={`${formId}-wd`}
            className="block text-[13px] font-medium text-[var(--text-primary)]"
          >
            Working directory
          </label>
          <p className="mt-0.5 mb-1 text-xs text-[var(--text-secondary)] leading-snug">
            Absolute path inside the container. Project folders are mounted under{" "}
            <code className="font-mono">/workspace</code>.
          </p>
          <input
            id={`${formId}-wd`}
            value={workingDir}
            onChange={(e) => setWorkingDir(e.target.value)}
            onBlur={() => setTouched((t) => ({ ...t, workingDir: true }))}
            placeholder={DEFAULT_WORKING_DIR}
            aria-invalid={show("workingDir") ? true : undefined}
            className={monoInputClass}
          />
          {errorText(show("workingDir"))}
        </div>

        {/* How a scheduled run actually behaves. */}
        <div className="rounded-[var(--radius-control)] border border-[var(--border-color)] bg-[var(--bg-secondary)] px-3 py-2 space-y-1">
          <p className="text-xs text-[var(--text-secondary)]">
            Scheduled runs are <strong className="text-[var(--text-primary)]">headless</strong> —
            the container executes <code className="font-mono">claude -p "…"</code> with no
            terminal attached, using this project&rsquo;s permission mode (
            <strong className="text-[var(--text-primary)]">{modeLabel}</strong>).
          </p>
          {mode !== "bypass" && (
            <p className="text-xs text-[var(--warning)]">
              A headless run cannot answer a permission prompt. In {modeLabel} mode the task may
              stall and produce an empty log; set the mode to Bypass in the Config tab for
              unattended runs.
            </p>
          )}
        </div>

        {task && (
          /*
            An edit is `add` then `remove` (see `update_scheduled_task`), and
            `triple-c-scheduler`'s remove now reaps the task's log directory —
            so on a current container the old logs are gone, not merely filed
            under the old id, which is what this used to promise.

            It is deliberately not stated as a certainty. `/usr/local/bin` only
            changes on base-image migration or Reset, so a project still running
            an older base image carries the older scheduler, whose remove leaves
            the log directory behind. "Assume they go with it" is true in both
            worlds and spares the user a paragraph about which one they are in.
          */
          <p className="text-xs text-[var(--text-secondary)]">
            The scheduler has no edit command, so saving re-creates this task under a new id and
            removes <code className="font-mono">{task.id}</code>. Assume its earlier run logs go
            with it.
          </p>
        )}

        {submitError && (
          <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap break-words">
            {submitError}
          </p>
        )}
      </form>
    </Modal>
  );
}
