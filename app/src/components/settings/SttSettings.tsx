import { useState, useEffect } from "react";
import { useSettings } from "../../hooks/useSettings";
import { getSttStatus, startStt, stopStt, pullSttImage, buildSttImage } from "../../lib/tauri-commands";
import { listen } from "@tauri-apps/api/event";
import type { SttStatus } from "../../lib/types";
import Tooltip from "../ui/Tooltip";
import Toggle from "../ui/Toggle";

export default function SttSettings() {
  const { appSettings, saveSettings } = useSettings();
  const [status, setStatus] = useState<SttStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [pulling, setPulling] = useState(false);
  const [building, setBuilding] = useState(false);
  const [buildLog, setBuildLog] = useState<string | null>(null);
  const [model, setModel] = useState(appSettings?.stt?.model ?? "tiny");
  const [port, setPort] = useState(String(appSettings?.stt?.port ?? 9876));
  const [language, setLanguage] = useState(appSettings?.stt?.language ?? "");

  useEffect(() => {
    setModel(appSettings?.stt?.model ?? "tiny");
    setPort(String(appSettings?.stt?.port ?? 9876));
    setLanguage(appSettings?.stt?.language ?? "");
  }, [appSettings?.stt?.model, appSettings?.stt?.port, appSettings?.stt?.language]);

  useEffect(() => {
    refreshStatus();
  }, []);

  const refreshStatus = () => {
    getSttStatus().then(setStatus).catch(console.error);
  };

  const handleToggleEnabled = async () => {
    if (!appSettings) return;
    const newEnabled = !appSettings.stt.enabled;
    await saveSettings({
      ...appSettings,
      stt: { ...appSettings.stt, enabled: newEnabled },
    });
  };

  const handleSaveModel = async () => {
    if (!appSettings) return;
    await saveSettings({
      ...appSettings,
      stt: { ...appSettings.stt, model },
    });
  };

  const handleSavePort = async () => {
    if (!appSettings) return;
    const portNum = parseInt(port, 10);
    if (isNaN(portNum) || portNum < 1 || portNum > 65535) return;
    await saveSettings({
      ...appSettings,
      stt: { ...appSettings.stt, port: portNum },
    });
  };

  const handleSaveLanguage = async () => {
    if (!appSettings) return;
    await saveSettings({
      ...appSettings,
      stt: { ...appSettings.stt, language: language || null },
    });
  };

  const handleStartStop = async () => {
    setLoading(true);
    try {
      if (status?.running) {
        await stopStt();
      } else {
        await startStt();
      }
      refreshStatus();
    } catch (e) {
      console.error("STT toggle failed:", e);
    } finally {
      setLoading(false);
    }
  };

  const handlePull = async () => {
    setPulling(true);
    setBuildLog(null);
    const unlisten = await listen<string>("stt-pull-progress", (event) => {
      setBuildLog(event.payload);
    });
    try {
      await pullSttImage();
      refreshStatus();
    } catch (e) {
      console.error("STT image pull failed:", e);
      setBuildLog(`Error: ${e}`);
    } finally {
      setPulling(false);
      unlisten();
    }
  };

  const handleBuild = async () => {
    setBuilding(true);
    setBuildLog(null);
    const unlisten = await listen<string>("stt-build-progress", (event) => {
      setBuildLog(event.payload);
    });
    try {
      await buildSttImage();
      refreshStatus();
    } catch (e) {
      console.error("STT image build failed:", e);
      setBuildLog(`Error: ${e}`);
    } finally {
      setBuilding(false);
      unlisten();
    }
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-1">
        Speech to Text
        <Tooltip text="Transcribe speech to text using Faster Whisper in a Docker container. Adds a mic button to the terminal." />
      </label>
      <p className="text-xs text-[var(--text-secondary)] mb-2">
        Click the mic button in the terminal to dictate text via speech recognition.
      </p>

      <div className="space-y-2">
        {/* Enable toggle */}
        <div className="flex items-center gap-2">
          <Toggle
            label="Speech to text"
            checked={!!appSettings?.stt?.enabled}
            onChange={handleToggleEnabled}
          />
          <span className="text-xs text-[var(--text-secondary)]">
            {appSettings?.stt?.enabled ? "Enabled" : "Disabled"}
          </span>
        </div>

        {appSettings?.stt?.enabled && (
          <>
            {/* Model selector */}
            <div>
              <label className="block text-xs text-[var(--text-secondary)] mb-1">Model</label>
              <select
                value={model}
                onChange={(e) => setModel(e.target.value)}
                onBlur={handleSaveModel}
                className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
              >
                <option value="tiny">Tiny (fastest, ~75MB)</option>
                <option value="small">Small (balanced, ~500MB)</option>
                <option value="medium">Medium (most accurate, ~1.5GB)</option>
              </select>
            </div>

            {/* Port */}
            <div>
              <label className="block text-xs text-[var(--text-secondary)] mb-1">Port</label>
              <input
                type="number"
                value={port}
                onChange={(e) => setPort(e.target.value)}
                onBlur={handleSavePort}
                min={1}
                max={65535}
                className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
              />
            </div>

            {/* Language */}
            <div>
              <label className="block text-xs text-[var(--text-secondary)] mb-1">Language (optional)</label>
              <input
                type="text"
                value={language}
                onChange={(e) => setLanguage(e.target.value)}
                onBlur={handleSaveLanguage}
                placeholder="Auto-detect"
                className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
              />
            </div>

            {/* Container status + controls */}
            <div className="pt-1">
              <label className="block text-xs text-[var(--text-secondary)] mb-1">STT Container</label>
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-xs text-[var(--text-secondary)]">
                  {status?.image_exists
                    ? status.running
                      ? `Running (port ${status.port}, model: ${status.model})`
                      : status.container_exists
                        ? "Stopped"
                        : "Image ready"
                    : "No image"}
                </span>
                {status?.image_exists && (
                  <button
                    onClick={handleStartStop}
                    disabled={loading}
                    className={`px-2 py-0.5 text-xs rounded transition-colors ${
                      status?.running
                        ? "text-[var(--error)] hover:bg-[var(--bg-primary)]"
                        : "text-[var(--success)] hover:bg-[var(--bg-primary)]"
                    }`}
                  >
                    {loading ? "..." : status?.running ? "Stop" : "Start"}
                  </button>
                )}
              </div>

              {/* Image actions */}
              <div className="flex items-center gap-2 mt-2">
                <button
                  onClick={handlePull}
                  disabled={pulling || building}
                  className="px-3 py-1 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] disabled:text-[var(--text-disabled)] transition-colors"
                >
                  {pulling ? "Pulling..." : "Pull Image"}
                </button>
                <button
                  onClick={handleBuild}
                  disabled={pulling || building}
                  className="px-3 py-1 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] disabled:text-[var(--text-disabled)] transition-colors"
                >
                  {building ? "Building..." : "Build Locally"}
                </button>
              </div>

              {buildLog && (
                <pre className="mt-2 text-[10px] text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded px-2 py-1 max-h-20 overflow-y-auto whitespace-pre-wrap">
                  {buildLog}
                </pre>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
