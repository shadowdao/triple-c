import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import GatewaySettings from "./GatewaySettings";
import type { AppSettings, GatewayStatus } from "../../lib/types";

const getGatewayStatus = vi.fn();
const stopGateway = vi.fn();
const startGateway = vi.fn();
const checkGatewayHealth = vi.fn();
const saveSettings = vi.fn();

vi.mock("../../lib/tauri-commands", () => ({
  getGatewayStatus: () => getGatewayStatus(),
  startGateway: () => startGateway(),
  stopGateway: () => stopGateway(),
  checkGatewayHealth: () => checkGatewayHealth(),
  pullGatewayImage: vi.fn(),
  buildGatewayImage: vi.fn(),
  setGatewayApiKey: vi.fn(),
  clearGatewayApiKey: vi.fn(),
  getGatewayAuthToken: vi.fn(),
  regenerateGatewayAuthToken: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));

let appSettings: AppSettings | null = null;
vi.mock("../../hooks/useSettings", () => ({
  useSettings: () => ({ appSettings, saveSettings }),
}));

const settingsWithGateway = (enabled: boolean): AppSettings =>
  ({
    gateway: { enabled, port: 4000, provider: "openai", api_base: null, models: [] },
  }) as unknown as AppSettings;

const status = (over: Partial<GatewayStatus> = {}): GatewayStatus => ({
  container_exists: true,
  running: true,
  port: 4000,
  image_exists: true,
  model_count: 0,
  has_api_key: false,
  base_url: "http://host.docker.internal:4000",
  ...over,
});

describe("GatewaySettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    appSettings = settingsWithGateway(false);
    getGatewayStatus.mockResolvedValue(status());
    checkGatewayHealth.mockResolvedValue(true);
    saveSettings.mockImplementation(async (s: AppSettings) => s);
    stopGateway.mockResolvedValue(undefined);
  });

  it("keeps a working Stop button when the gateway is disabled but its container exists", async () => {
    render(<GatewaySettings />);

    const stop = await screen.findByRole("button", { name: "Stop" });
    // The configuration UI stays hidden — only the container row survives.
    expect(screen.queryByLabelText("Provider")).not.toBeInTheDocument();
    expect(screen.getByTestId("gateway-leftover-container")).toHaveTextContent(
      /gateway container is still present/i,
    );
    // Status is a word, not just a colour.
    expect(screen.getByTestId("gateway-leftover-container")).toHaveTextContent(
      /Running on port 4000/,
    );

    fireEvent.click(stop);
    await waitFor(() => expect(stopGateway).toHaveBeenCalledTimes(1));
    // Stopping re-reads status: once on mount, once after the action.
    await waitFor(() => expect(getGatewayStatus).toHaveBeenCalledTimes(2));
  });

  it("shows nothing extra when the gateway is disabled and no container exists", async () => {
    getGatewayStatus.mockResolvedValue(status({ container_exists: false, running: false }));
    render(<GatewaySettings />);

    await waitFor(() => expect(getGatewayStatus).toHaveBeenCalled());
    expect(screen.queryByTestId("gateway-leftover-container")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
  });

  it("re-reads container status after toggling the gateway off", async () => {
    appSettings = settingsWithGateway(true);
    render(<GatewaySettings />);

    await waitFor(() => expect(getGatewayStatus).toHaveBeenCalledTimes(1));

    // The backend stops the container as part of update_settings, so the UI has
    // to re-read rather than trust the status it already has.
    getGatewayStatus.mockResolvedValue(status({ running: false }));
    fireEvent.click(screen.getByRole("switch", { name: "Model gateway" }));

    await waitFor(() =>
      expect(saveSettings).toHaveBeenCalledWith(
        expect.objectContaining({ gateway: expect.objectContaining({ enabled: false }) }),
      ),
    );
    await waitFor(() => expect(getGatewayStatus).toHaveBeenCalledTimes(2));
  });
});
