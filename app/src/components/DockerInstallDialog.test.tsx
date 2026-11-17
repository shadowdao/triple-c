import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import DockerInstallDialog from "./DockerInstallDialog";
import { useAppState } from "../store/appState";
import type { DockerStatus } from "../lib/types";

vi.mock("../lib/tauri-commands", () => ({
  getDockerStatus: vi.fn(),
  checkImageExists: vi.fn(),
  buildImage: vi.fn(),
  pullImage: vi.fn(),
  openUrlExternal: vi.fn(),
}));

vi.mock("../hooks/useInstallHelper", () => ({
  useInstallHelper: () => ({
    options: {
      os: "linux",
      product_name: "Docker Engine",
      can_auto_install: false,
      auto_install_method: null,
      auto_install_blocker: "no pkexec",
      docs_url: "https://docs.docker.com",
      manual_steps: [],
      post_install_notes: [],
    },
    loadOptions: vi.fn(),
    runInstall: vi.fn(),
  }),
}));

const status = (state: DockerStatus["state"], os: DockerStatus["os"] = "linux"): DockerStatus => ({
  state,
  endpoint: "unix:///var/run/docker.sock",
  detail: state === "running" ? null : "connection refused",
  os,
});

function setStatus(s: DockerStatus) {
  act(() => useAppState.getState().setDockerStatus(s));
}

describe("DockerInstallDialog", () => {
  beforeEach(() => {
    useAppState.setState({ dockerStatus: null, dockerAvailable: null });
  });

  it("offers install guidance when Docker is not installed", () => {
    setStatus(status("not_installed"));
    render(<DockerInstallDialog onClose={vi.fn()} />);
    expect(screen.getByRole("dialog", { name: "Docker is not installed" })).toBeTruthy();
    expect(screen.getByText(/AWS projects don't need local Docker/)).toBeTruthy();
    expect(screen.getByText("unix:///var/run/docker.sock")).toBeTruthy();
  });

  it("says installed-but-not-running with systemd steps on Linux, and offers no install", () => {
    setStatus(status("not_running"));
    render(<DockerInstallDialog onClose={vi.fn()} />);
    expect(screen.getByRole("dialog", { name: "Docker is installed but not running" })).toBeTruthy();
    expect(screen.getByText("sudo systemctl start docker")).toBeTruthy();
    expect(screen.getByText("sudo systemctl reset-failed docker")).toBeTruthy();
    expect(screen.getByText("journalctl -u docker -b")).toBeTruthy();
    expect(screen.queryByText(/Install Docker Engine/)).toBeNull();
    expect(screen.getByText(/AWS projects don't need local Docker/)).toBeTruthy();
  });

  it("tells macOS/Windows users to start Docker Desktop", () => {
    setStatus(status("not_running", "macos"));
    render(<DockerInstallDialog onClose={vi.fn()} />);
    expect(screen.getByText("Docker Desktop")).toBeTruthy();
    expect(screen.queryByText("sudo systemctl start docker")).toBeNull();
  });

  it("explains the docker group on permission denied", () => {
    setStatus(status("no_permission"));
    render(<DockerInstallDialog onClose={vi.fn()} />);
    expect(screen.getByRole("dialog", { name: "Docker is running but not accessible" })).toBeTruthy();
    expect(screen.getByText("sudo usermod -aG docker $USER")).toBeTruthy();
  });

  it("updates live and closes itself when Docker becomes available", () => {
    setStatus(status("not_installed"));
    const onClose = vi.fn();
    render(<DockerInstallDialog onClose={onClose} />);
    setStatus(status("not_running"));
    expect(screen.getByRole("dialog", { name: "Docker is installed but not running" })).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    setStatus(status("running"));
    expect(onClose).toHaveBeenCalled();
  });
});
