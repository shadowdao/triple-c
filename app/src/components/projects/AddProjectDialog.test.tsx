import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import AddProjectDialog from "./AddProjectDialog";
import { useAppState } from "../../store/appState";
import type { AppSettings, EnvironmentScan } from "../../lib/types";
import { environment } from "../../test/awsFixtures";

const add = vi.fn();

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ add }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
}));

const detectAwsEnvironments = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  detectAwsEnvironments: (c: string, r: string) => detectAwsEnvironments(c, r),
}));

/** A promise whose resolution this test controls, so `loading` can be held open. */
function deferred() {
  let resolve!: (v: unknown) => void;
  const promise = new Promise((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function fillValidForm() {
  fireEvent.change(screen.getByLabelText("Project name"), {
    target: { value: "my-project" },
  });
  fireEvent.change(screen.getByLabelText("Folder 1 host path"), {
    target: { value: "/home/user/my-project" },
  });
}

function submitButton() {
  return screen.getByRole("button", { name: /Add Project|Adding/ });
}

describe("AddProjectDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("adds the project with the name and folder entered", async () => {
    add.mockResolvedValue({ id: "p1" });
    const onClose = vi.fn();
    render(<AddProjectDialog onClose={onClose} />);
    fillValidForm();
    fireEvent.click(submitButton());
    await waitFor(() =>
      expect(add).toHaveBeenCalledWith("my-project", [
        { host_path: "/home/user/my-project", mount_name: "my-project" },
      ]),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("keeps the submit button announced, and explains why, while adding", async () => {
    const { promise, resolve } = deferred();
    add.mockReturnValue(promise);
    render(<AddProjectDialog onClose={vi.fn()} />);
    fillValidForm();
    fireEvent.click(submitButton());

    // Native `disabled` would remove the button from the accessibility tree
    // exactly when it has something to say.
    await waitFor(() =>
      expect(submitButton()).toHaveAttribute("aria-disabled", "true"),
    );
    expect(submitButton()).not.toBeDisabled();
    expect(submitButton()).toHaveAccessibleDescription(/being added/i);

    await act(async () => resolve({ id: "p1" }));
  });

  it("ignores clicks and Enter/Space on the submit button while adding", async () => {
    const { promise, resolve } = deferred();
    add.mockReturnValue(promise);
    render(<AddProjectDialog onClose={vi.fn()} />);
    fillValidForm();
    fireEvent.click(submitButton());
    await waitFor(() =>
      expect(submitButton()).toHaveAttribute("aria-disabled", "true"),
    );

    fireEvent.click(submitButton());
    fireEvent.keyDown(submitButton(), { key: "Enter" });
    fireEvent.keyDown(submitButton(), { key: " " });
    expect(add).toHaveBeenCalledTimes(1);

    await act(async () => resolve({ id: "p1" }));
  });

  it("ignores a form submit raised from elsewhere while adding", async () => {
    const { promise, resolve } = deferred();
    add.mockReturnValue(promise);
    render(<AddProjectDialog onClose={vi.fn()} />);
    fillValidForm();
    fireEvent.click(submitButton());
    await waitFor(() =>
      expect(submitButton()).toHaveAttribute("aria-disabled", "true"),
    );

    // Enter in a text field submits a form regardless of the submit button's
    // state, so the handler has to guard itself too.
    // Modal portals to document.body, so the form is not under `container`.
    const form = document.querySelector("form");
    expect(form).not.toBeNull();
    fireEvent.submit(form!);
    expect(add).toHaveBeenCalledTimes(1);

    await act(async () => resolve({ id: "p1" }));
  });

  it("leaves the submit button plainly available when idle", () => {
    render(<AddProjectDialog onClose={vi.fn()} />);
    expect(submitButton()).not.toHaveAttribute("aria-disabled");
    expect(submitButton()).toHaveAccessibleDescription("");
  });
});

describe("AddProjectDialog on AWS", () => {
  const scan = (envs: EnvironmentScan["environments"]): EnvironmentScan => ({
    region: "eu-west-1",
    account: "111122223333",
    owner_id: "AROA:alice",
    owner_key: "abc",
    state_bucket: "yes",
    boundary: "yes",
    environments: envs,
  });

  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      appSettings: {
        hosts: [
          {
            id: "c1",
            name: "Work account",
            kind: { type: "aws", auth: { method: "profile", name: "work" }, default_region: "eu-west-1" },
          },
        ],
      } as unknown as AppSettings,
    });
  });

  function chooseAws() {
    fireEvent.change(screen.getByLabelText("Runs on"), { target: { value: "c1" } });
  }

  it("sends the host, placement and workspace with repositories instead of folders", async () => {
    detectAwsEnvironments.mockResolvedValue(
      scan([environment("staging", "deploying", true), environment("dev", "ready", true)]),
    );
    add.mockResolvedValue({ id: "p1" });
    const onClose = vi.fn();
    render(<AddProjectDialog onClose={onClose} />);
    chooseAws();
    expect(screen.getByLabelText("Region")).toHaveValue("eu-west-1");
    await waitFor(() => expect(screen.getByLabelText("Environment")).toHaveValue("dev"));
    expect(detectAwsEnvironments).toHaveBeenCalledWith("c1", "eu-west-1");
    expect(
      (screen.getByRole("option", { name: /staging/ }) as HTMLOptionElement).disabled,
    ).toBe(true);
    expect(screen.queryByLabelText("Folder 1 host path")).toBeNull();

    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "api" } });
    fireEvent.change(screen.getByLabelText("Repository 1 URL"), {
      target: { value: "git@github.com:org/api.git" },
    });
    fireEvent.change(screen.getByLabelText("Repository 1 branch"), { target: { value: "develop" } });
    fireEvent.click(submitButton());

    await waitFor(() => expect(add).toHaveBeenCalled());
    const [name, paths, host] = add.mock.calls[0];
    expect(name).toBe("api");
    expect(paths).toEqual([]);
    expect(host.hostId).toBe("c1");
    expect(host.awsPlacement).toEqual({ region: "eu-west-1", environment: "dev" });
    expect(host.aws.repos).toEqual([
      { url: "git@github.com:org/api.git", branch: "develop", folder: "api" },
    ]);
    expect(host.aws).toMatchObject({ cpu: 1024, memory_mib: 4096, wip_push_on_stop: true, task_arn: null });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("points at Settings → Hosts when no environment is usable", async () => {
    detectAwsEnvironments.mockResolvedValue(scan([environment("dev", "ready", false)]));
    render(<AddProjectDialog onClose={vi.fn()} />);
    chooseAws();
    expect(await screen.findByText(/Settings → Hosts/)).toBeInTheDocument();
    expect(screen.queryByLabelText("Environment")).toBeNull();
    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "api" } });
    fireEvent.click(submitButton());
    expect(await screen.findByRole("alert")).toHaveTextContent(/Choose an environment/);
    expect(add).not.toHaveBeenCalled();
  });

  it("shows the backend's validation message when the add is refused", async () => {
    detectAwsEnvironments.mockResolvedValue(scan([environment("dev", "update_available", true)]));
    add.mockRejectedValue('Repository URL "ftp://x": use https://..., ssh://... or git@host:path');
    render(<AddProjectDialog onClose={vi.fn()} />);
    chooseAws();
    await waitFor(() => expect(screen.getByLabelText("Environment")).toHaveValue("dev"));
    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "api" } });
    fireEvent.change(screen.getByLabelText("Repository 1 URL"), { target: { value: "ftp://x" } });
    fireEvent.click(submitButton());
    expect(await screen.findByRole("alert")).toHaveTextContent(/use https:\/\//);
  });

  it("forgets the environment when the region changes until that region's scan is done", async () => {
    detectAwsEnvironments.mockResolvedValueOnce(scan([environment("dev", "ready", true)]));
    let finish!: (v: EnvironmentScan) => void;
    detectAwsEnvironments.mockReturnValueOnce(new Promise((r) => (finish = r)));
    render(<AddProjectDialog onClose={vi.fn()} />);
    chooseAws();
    await waitFor(() => expect(screen.getByLabelText("Environment")).toHaveValue("dev"));
    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "api" } });
    const region = screen.getByLabelText("Region");
    fireEvent.change(region, { target: { value: "us-east-1" } });
    fireEvent.blur(region);
    expect(await screen.findByText(/Looking for environments in us-east-1/)).toBeInTheDocument();
    fireEvent.click(submitButton());
    expect(await screen.findByRole("alert")).toHaveTextContent(/Choose an environment/);
    expect(add).not.toHaveBeenCalled();
    await act(async () => finish(scan([environment("prod", "ready", true)])));
    expect(screen.getByLabelText("Environment")).toHaveValue("prod");
  });
});
