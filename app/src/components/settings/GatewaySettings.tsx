import { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { useSettings } from "../../hooks/useSettings";
import {
  getGatewayStatus,
  startGateway,
  stopGateway,
  checkGatewayHealth,
  pullGatewayImage,
  buildGatewayImage,
  setGatewayApiKey,
  clearGatewayApiKey,
  getGatewayAuthToken,
  regenerateGatewayAuthToken,
} from "../../lib/tauri-commands";
import type { GatewayModel, GatewaySettings as GatewaySettingsType, GatewayStatus } from "../../lib/types";
import Button from "../ui/Button";
import Field, { SwitchRow, inputClass, monoInputClass } from "../ui/Field";
import Modal from "../ui/Modal";
import StatusIndicator, { type StatusTone } from "../ui/StatusIndicator";
import Toggle from "../ui/Toggle";

const DEFAULT_GATEWAY: GatewaySettingsType = {
  enabled: false,
  port: 4000,
  provider: "openai",
  api_base: null,
  models: [],
};

/**
 * Settings for the model gateway — the LiteLLM container Triple-C runs so that
 * Claude Code, which only speaks the Anthropic Messages API, can be driven by
 * an OpenAI key.
 *
 * The provider API key is write-only from here: it goes to the OS keychain and
 * there is no command that reads it back, so the UI can only ever report
 * whether one is stored.
 */
export default function GatewaySettings() {
  const { appSettings, saveSettings } = useSettings();
  const gateway = appSettings?.gateway ?? DEFAULT_GATEWAY;

  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [healthy, setHealthy] = useState<boolean | null>(null);
  const [loading, setLoading] = useState(false);
  const [pulling, setPulling] = useState(false);
  const [building, setBuilding] = useState(false);
  const [log, setLog] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [provider, setProvider] = useState(gateway.provider);
  const [port, setPort] = useState(String(gateway.port));
  const [apiBase, setApiBase] = useState(gateway.api_base ?? "");
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [savingKey, setSavingKey] = useState(false);

  const [authToken, setAuthToken] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [confirmRotate, setConfirmRotate] = useState(false);

  useEffect(() => {
    setProvider(gateway.provider);
    setPort(String(gateway.port));
    setApiBase(gateway.api_base ?? "");
  }, [gateway.provider, gateway.port, gateway.api_base]);

  const refreshStatus = useCallback(async () => {
    try {
      const next = await getGatewayStatus();
      setStatus(next);
      setHealthy(next.running ? await checkGatewayHealth() : null);
    } catch (e) {
      console.error("Gateway status failed:", e);
    }
  }, []);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  const patch = async (changes: Partial<GatewaySettingsType>) => {
    if (!appSettings) return;
    await saveSettings({ ...appSettings, gateway: { ...gateway, ...changes } });
  };

  const savePort = async () => {
    const parsed = parseInt(port, 10);
    if (isNaN(parsed) || parsed < 1 || parsed > 65535) {
      setPort(String(gateway.port));
      return;
    }
    await patch({ port: parsed });
  };

  const setModels = (models: GatewayModel[]) => patch({ models });

  const updateModel = (index: number, changes: Partial<GatewayModel>) =>
    setModels(gateway.models.map((m, i) => (i === index ? { ...m, ...changes } : m)));

  const run = async (fn: () => Promise<unknown>) => {
    setLoading(true);
    setError(null);
    try {
      await fn();
      await refreshStatus();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const withProgress = async (
    event: string,
    setBusy: (busy: boolean) => void,
    fn: () => Promise<void>,
  ) => {
    setBusy(true);
    setLog(null);
    setError(null);
    const unlisten = await listen<string>(event, (e) => setLog(e.payload));
    try {
      await fn();
      await refreshStatus();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      unlisten();
    }
  };

  const handleSaveKey = async () => {
    if (!apiKeyDraft.trim()) return;
    setSavingKey(true);
    setError(null);
    try {
      await setGatewayApiKey(apiKeyDraft);
      setApiKeyDraft("");
      await refreshStatus();
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingKey(false);
    }
  };

  const revealToken = async () => {
    try {
      setAuthToken(await getGatewayAuthToken());
    } catch (e) {
      setError(String(e));
    }
  };

  const rotateToken = async () => {
    setConfirmRotate(false);
    try {
      setAuthToken(await regenerateGatewayAuthToken());
      await refreshStatus();
    } catch (e) {
      setError(String(e));
    }
  };

  const copy = async (label: string, value: string) => {
    await navigator.clipboard.writeText(value);
    setCopied(label);
    setTimeout(() => setCopied(null), 2000);
  };

  const tone: StatusTone = !status?.image_exists
    ? "off"
    : status.running
      ? healthy === false
        ? "busy"
        : "running"
      : status.container_exists
        ? "stopped"
        : "off";

  const statusLabel = !status?.image_exists
    ? "No image"
    : status.running
      ? healthy === false
        ? "Starting…"
        : `Running on port ${status.port}`
      : status.container_exists
        ? "Stopped"
        : "Image ready";

  return (
    <div>
      <label className="block text-sm font-medium mb-1">Model Gateway</label>
      <p className="text-xs text-[var(--text-secondary)] mb-3">
        Runs a pinned LiteLLM proxy in a container. Claude Code only speaks the Anthropic
        Messages API, so an OpenAI key cannot drive it directly — the gateway serves{" "}
        <code className="font-mono">/v1/messages</code> and translates each call to your
        provider. Point a project's <strong>OpenAI Compatible</strong> backend at it.
      </p>

      <div className="space-y-4">
        <SwitchRow
          label="Model gateway"
          hint="Start the gateway container with Triple-C."
          control={
            <Toggle
              label="Model gateway"
              checked={gateway.enabled}
              onChange={(value) => patch({ enabled: value })}
            />
          }
        />

        {gateway.enabled && (
          <>
            {/* ── Container ─────────────────────────────────────────────── */}
            <div className="flex items-center gap-3 flex-wrap">
              <StatusIndicator tone={tone} label={statusLabel} className="text-xs" />
              {status?.image_exists && (
                <Button
                  variant={status.running ? "danger" : "primary"}
                  disabled={loading}
                  onClick={() => run(status.running ? stopGateway : startGateway)}
                >
                  {loading ? "Working…" : status.running ? "Stop" : "Start"}
                </Button>
              )}
              <Button
                disabled={pulling || building}
                onClick={() => withProgress("gateway-pull-progress", setPulling, pullGatewayImage)}
              >
                {pulling ? "Pulling…" : "Pull Image"}
              </Button>
              <Button
                disabled={pulling || building}
                onClick={() => withProgress("gateway-build-progress", setBuilding, buildGatewayImage)}
              >
                {building ? "Building…" : "Build Locally"}
              </Button>
            </div>

            {log && (
              <pre className="text-[10px] text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] px-2 py-1 max-h-20 overflow-y-auto whitespace-pre-wrap">
                {log}
              </pre>
            )}

            {error && (
              <p className="text-xs text-[var(--error)]" role="alert">
                {error}
              </p>
            )}

            {/* ── Provider ──────────────────────────────────────────────── */}
            <Field
              label="Provider"
              hint="LiteLLM provider prefix. OpenAI is the common case; anything LiteLLM supports works (azure, gemini, groq, …)."
            >
              {(id) => (
                <input
                  id={id}
                  type="text"
                  value={provider}
                  onChange={(e) => setProvider(e.target.value)}
                  onBlur={() => patch({ provider: provider.trim() || "openai" })}
                  placeholder="openai"
                  className={inputClass}
                />
              )}
            </Field>

            <Field
              label="Provider API key"
              hint={
                status?.has_api_key
                  ? "A key is stored in your OS keychain. Enter a new one to replace it — it is never shown again."
                  : "Stored in your OS keychain, written only into the gateway container's config. Never shown again once saved."
              }
            >
              {(id) => (
                <div className="flex items-center gap-2">
                  <input
                    id={id}
                    type="password"
                    autoComplete="off"
                    value={apiKeyDraft}
                    onChange={(e) => setApiKeyDraft(e.target.value)}
                    placeholder={status?.has_api_key ? "•••••••• (stored)" : "sk-…"}
                    className={monoInputClass}
                  />
                  <Button
                    variant="primary"
                    disabled={savingKey || !apiKeyDraft.trim()}
                    onClick={handleSaveKey}
                  >
                    {savingKey ? "Saving…" : "Save"}
                  </Button>
                  {status?.has_api_key && (
                    <Button variant="danger" onClick={() => run(clearGatewayApiKey)}>
                      Clear
                    </Button>
                  )}
                </div>
              )}
            </Field>

            <Field
              label="Provider base URL (optional)"
              hint="Override the provider's endpoint — Azure deployments, self-hosted OpenAI-compatible servers, and so on. Leave blank for the provider default."
            >
              {(id) => (
                <input
                  id={id}
                  type="text"
                  value={apiBase}
                  onChange={(e) => setApiBase(e.target.value)}
                  onBlur={() => patch({ api_base: apiBase.trim() || null })}
                  placeholder="https://api.openai.com/v1"
                  className={inputClass}
                />
              )}
            </Field>

            <Field
              label="Host port"
              hint="Port the gateway is published on. Changing it recreates the container."
            >
              {(id) => (
                <input
                  id={id}
                  type="number"
                  min={1}
                  max={65535}
                  value={port}
                  onChange={(e) => setPort(e.target.value)}
                  onBlur={savePort}
                  className={inputClass}
                />
              )}
            </Field>

            {/* ── Models ────────────────────────────────────────────────── */}
            <div>
              <div className="text-[13px] font-medium text-[var(--text-primary)]">Models</div>
              <p className="mt-0.5 mb-2 text-xs text-[var(--text-secondary)] leading-snug">
                Each row becomes one model the gateway serves. <strong>Name</strong> is what a
                project puts in its model field; <strong>Model id</strong> is the provider's own
                id. The gateway sends them as{" "}
                <code className="font-mono">{provider || "openai"}/&lt;model id&gt;</code>.
              </p>

              <div className="space-y-2">
                {gateway.models.map((model, index) => (
                  <div key={index} className="flex items-center gap-2">
                    <input
                      type="text"
                      aria-label={`Model ${index + 1} name`}
                      value={model.name}
                      onChange={(e) => updateModel(index, { name: e.target.value })}
                      placeholder="gpt-5.1"
                      className={monoInputClass}
                    />
                    <input
                      type="text"
                      aria-label={`Model ${index + 1} provider id`}
                      value={model.model_id}
                      onChange={(e) => updateModel(index, { model_id: e.target.value })}
                      placeholder="gpt-5.1"
                      className={monoInputClass}
                    />
                    <Button
                      variant="ghost"
                      aria-label={`Remove model ${index + 1}`}
                      onClick={() => setModels(gateway.models.filter((_, i) => i !== index))}
                    >
                      Remove
                    </Button>
                  </div>
                ))}
                <Button
                  onClick={() => setModels([...gateway.models, { name: "", model_id: "" }])}
                >
                  Add model
                </Button>
              </div>
            </div>

            {/* ── What a project should use ─────────────────────────────── */}
            <div className="border border-[var(--border-color)] rounded-[var(--radius-panel)] bg-[var(--bg-secondary)] px-3 py-3 space-y-3">
              <div>
                <div className="text-[13px] font-medium text-[var(--text-primary)]">
                  Project settings for this gateway
                </div>
                <p className="mt-0.5 text-xs text-[var(--text-secondary)] leading-snug">
                  Set a project's backend to <strong>OpenAI Compatible</strong> and use these
                  values. On native Linux Docker, where{" "}
                  <code className="font-mono">host.docker.internal</code> is not injected into
                  containers, use <code className="font-mono">http://172.17.0.1:{gateway.port}</code>{" "}
                  instead.
                </p>
              </div>

              <Field label="Base URL">
                {(id) => (
                  <div className="flex items-center gap-2">
                    <input
                      id={id}
                      readOnly
                      value={status?.base_url ?? `http://host.docker.internal:${gateway.port}`}
                      className={monoInputClass}
                    />
                    <Button
                      onClick={() =>
                        copy(
                          "url",
                          status?.base_url ?? `http://host.docker.internal:${gateway.port}`,
                        )
                      }
                    >
                      {copied === "url" ? "Copied" : "Copy"}
                    </Button>
                  </div>
                )}
              </Field>

              <Field
                label="Auth token"
                hint="The gateway requires this on every request, which is what stops the published port being an open proxy onto your provider account."
              >
                {(id) => (
                  <div className="flex items-center gap-2">
                    <input
                      id={id}
                      readOnly
                      type={authToken ? "text" : "password"}
                      value={authToken ?? "••••••••••••"}
                      className={monoInputClass}
                    />
                    {authToken ? (
                      <Button onClick={() => copy("token", authToken)}>
                        {copied === "token" ? "Copied" : "Copy"}
                      </Button>
                    ) : (
                      <Button onClick={revealToken}>Reveal</Button>
                    )}
                    <Button variant="danger" onClick={() => setConfirmRotate(true)}>
                      Regenerate
                    </Button>
                  </div>
                )}
              </Field>
            </div>
          </>
        )}
      </div>

      {confirmRotate && (
        <Modal
          title="Regenerate gateway auth token?"
          onClose={() => setConfirmRotate(false)}
          footer={
            <div className="flex justify-end gap-2">
              <Button size="md" onClick={() => setConfirmRotate(false)}>
                Cancel
              </Button>
              <Button size="md" variant="danger" onClick={rotateToken}>
                Regenerate
              </Button>
            </div>
          }
        >
          <p className="text-[13px] text-[var(--text-secondary)]">
            Every project still using the current token will stop reaching the gateway until you
            paste the new one into its model config. The gateway is recreated on its next start.
          </p>
        </Modal>
      )}
    </div>
  );
}
