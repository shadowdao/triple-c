import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import TaskEditorModal from "./TaskEditorModal";
import type { Project, ScheduledTask } from "../../../lib/types";

const addScheduledTask = vi.fn(async () => "a1b2c3d4");
const updateScheduledTask = vi.fn(async () => "e5f6a7b8");

vi.mock("../../../lib/tauri-commands", () => ({
  addScheduledTask: (...args: unknown[]) => addScheduledTask(...(args as [])),
  updateScheduledTask: (...args: unknown[]) => updateScheduledTask(...(args as [])),
}));

/** Modal focuses via rAF; jsdom needs a flush. */
async function flushFocus() {
  await act(async () => {
    vi.advanceTimersByTime(20);
  });
}

const baseProject: Project = {
  id: "p1",
  name: "api-server",
  paths: [{ host_path: "/home/user/api", mount_name: "api" }],
  container_id: "c1",
  status: "running",
  backend: "anthropic",
  bedrock_config: null,
  ollama_config: null,
  openai_compatible_config: null,
  allow_docker_access: false,
  sandbox_mode_enabled: true,
  mission_control_enabled: false,
  auth_bridge_enabled: false,
  use_shared_auth_token: true,
  full_permissions: false,
  permission_mode: "bypass",
  ssh_key_path: null,
  git_token: null,
  git_user_name: null,
  git_user_email: null,
  custom_env_vars: [],
  port_mappings: [],
  claude_instructions: null,
  claude_code_settings: null,
  renamed_session_names: {},
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

const existingTask: ScheduledTask = {
  id: "a1b2c3d4",
  name: "nightly",
  prompt: "Run the suite",
  schedule: "0 3 * * *",
  task_type: "recurring",
  at: null,
  enabled: false,
  working_dir: "/workspace/api",
  created_at: null,
  last_run: null,
  next_run: null,
  running: false,
  running_since: null,
};

async function renderEditor(task: ScheduledTask | null = null, project = baseProject) {
  const onClose = vi.fn();
  const onSaved = vi.fn();
  render(
    <TaskEditorModal project={project} task={task} onClose={onClose} onSaved={onSaved} />,
  );
  await flushFocus();
  return { onClose, onSaved };
}

const field = (name: RegExp) => screen.getByLabelText(name) as HTMLInputElement;
const submit = async () =>
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: /create task|save changes/i }));
  });

describe("TaskEditorModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "setTimeout"] });
  });
  afterEach(() => vi.useRealTimers());

  it("sends the typed values through as data, untouched", async () => {
    await renderEditor();
    fireEvent.change(field(/^name$/i), { target: { value: "  nightly  " } });
    // A prompt full of shell syntax must reach the backend verbatim.
    fireEvent.change(field(/^prompt$/i), {
      target: { value: 'echo "hi"; rm -rf / $(id)\nsecond line' },
    });
    fireEvent.change(field(/cron expression/i), { target: { value: "0 3 * * *" } });
    await submit();

    expect(addScheduledTask).toHaveBeenCalledWith("p1", {
      name: "nightly",
      prompt: 'echo "hi"; rm -rf / $(id)\nsecond line',
      scheduleKind: "recurring",
      schedule: "0 3 * * *",
      workingDir: "/workspace",
    });
  });

  it("refuses to submit an invalid cron expression and says why", async () => {
    const { onSaved } = await renderEditor();
    fireEvent.change(field(/^name$/i), { target: { value: "nightly" } });
    fireEvent.change(field(/^prompt$/i), { target: { value: "do the thing" } });
    fireEvent.change(field(/cron expression/i), { target: { value: "99 * * * *" } });
    await submit();

    expect(addScheduledTask).not.toHaveBeenCalled();
    expect(onSaved).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent(/out of range for the minute field/i);
  });

  it("refuses a relative working directory", async () => {
    await renderEditor();
    fireEvent.change(field(/^name$/i), { target: { value: "nightly" } });
    fireEvent.change(field(/^prompt$/i), { target: { value: "do the thing" } });
    fireEvent.change(field(/working directory/i), { target: { value: "relative/path" } });
    await submit();

    expect(addScheduledTask).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent(/absolute path/i);
  });

  it("reads the cron expression back in English", async () => {
    await renderEditor();
    fireEvent.change(field(/cron expression/i), { target: { value: "0 9 * * 1-5" } });
    expect(screen.getByText("At 09:00, on Monday to Friday.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Hourly" }));
    expect(field(/cron expression/i).value).toBe("0 * * * *");
    expect(screen.getByText("At :00 past every hour, every day.")).toBeInTheDocument();
  });

  it("switches to a one-shot time and validates its format", async () => {
    await renderEditor();
    fireEvent.change(field(/^name$/i), { target: { value: "one-off" } });
    fireEvent.change(field(/^prompt$/i), { target: { value: "commit" } });
    fireEvent.click(screen.getByRole("radio", { name: "Once" }));

    fireEvent.change(field(/run at/i), { target: { value: "tomorrow" } });
    await submit();
    expect(addScheduledTask).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent(/YYYY-MM-DD HH:MM/);

    fireEvent.change(field(/run at/i), { target: { value: "2099-12-25 09:05" } });
    await submit();
    expect(addScheduledTask).toHaveBeenCalledWith(
      "p1",
      expect.objectContaining({ scheduleKind: "once", schedule: "2099-12-25 09:05" }),
    );
  });

  it("warns that a headless run cannot answer a permission prompt", async () => {
    // Bypass is the only mode where an unattended run is safe from stalling.
    await renderEditor(null, { ...baseProject, permission_mode: "bypass" });
    expect(screen.getByText(/headless/i)).toBeInTheDocument();
    expect(screen.queryByText(/cannot answer a permission prompt/i)).toBeNull();
  });

  it("spells out the stall risk in any non-Bypass mode", async () => {
    await renderEditor(null, { ...baseProject, permission_mode: "default" });
    expect(screen.getByText(/cannot answer a permission prompt/i)).toBeInTheDocument();
  });

  it("edits an existing task, carrying its enabled state and warning about the new id", async () => {
    const { onSaved, onClose } = await renderEditor(existingTask);
    expect(field(/^name$/i).value).toBe("nightly");
    expect(field(/cron expression/i).value).toBe("0 3 * * *");
    expect(field(/working directory/i).value).toBe("/workspace/api");
    // The id changes on edit; the user is told before they save.
    expect(screen.getByText(/re-creates this task under a new id/i)).toBeInTheDocument();

    fireEvent.change(field(/^name$/i), { target: { value: "nightly-v2" } });
    await submit();

    expect(updateScheduledTask).toHaveBeenCalledWith(
      "p1",
      "a1b2c3d4",
      expect.objectContaining({ name: "nightly-v2", workingDir: "/workspace/api" }),
      false, // the task was disabled and must not come back enabled
    );
    expect(onSaved).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("surfaces a backend rejection instead of closing", async () => {
    addScheduledTask.mockRejectedValueOnce(new Error("Container is not running"));
    const { onClose } = await renderEditor();
    fireEvent.change(field(/^name$/i), { target: { value: "nightly" } });
    fireEvent.change(field(/^prompt$/i), { target: { value: "do the thing" } });
    await submit();

    expect(screen.getByRole("alert")).toHaveTextContent(/Container is not running/);
    expect(onClose).not.toHaveBeenCalled();
  });
});
