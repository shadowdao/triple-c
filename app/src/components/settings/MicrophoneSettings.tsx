import { useState, useEffect, useCallback } from "react";
import { useSettings } from "../../hooks/useSettings";

interface AudioDevice {
  deviceId: string;
  label: string;
}

export default function MicrophoneSettings() {
  const { appSettings, saveSettings } = useSettings();
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [selected, setSelected] = useState(appSettings?.default_microphone ?? "");
  const [loading, setLoading] = useState(false);
  const [permissionNeeded, setPermissionNeeded] = useState(false);

  // Sync local state when appSettings change
  useEffect(() => {
    setSelected(appSettings?.default_microphone ?? "");
  }, [appSettings?.default_microphone]);

  const enumerateDevices = useCallback(async () => {
    setLoading(true);
    setPermissionNeeded(false);
    try {
      // Request mic permission first so device labels are available
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.getTracks().forEach((t) => t.stop());

      const allDevices = await navigator.mediaDevices.enumerateDevices();
      const mics = allDevices
        .filter((d) => d.kind === "audioinput")
        .map((d) => ({
          deviceId: d.deviceId,
          label: d.label || `Microphone (${d.deviceId.slice(0, 8)}...)`,
        }));
      setDevices(mics);
    } catch {
      setPermissionNeeded(true);
    } finally {
      setLoading(false);
    }
  }, []);

  // Enumerate devices on mount
  useEffect(() => {
    enumerateDevices();
  }, [enumerateDevices]);

  const handleChange = async (deviceId: string) => {
    setSelected(deviceId);
    if (appSettings) {
      await saveSettings({ ...appSettings, default_microphone: deviceId || null });
    }
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-1">Microphone</label>
      <p className="text-xs text-[var(--text-secondary)] mb-1.5">
        Audio input device for Claude Code voice mode (/voice)
      </p>
      {permissionNeeded ? (
        <div className="flex items-center gap-2">
          <span className="text-xs text-[var(--text-secondary)]">
            Microphone permission required
          </span>
          <button
            onClick={enumerateDevices}
            className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
          >
            Grant Access
          </button>
        </div>
      ) : (
        <div className="flex items-center gap-2">
          <select
            value={selected}
            onChange={(e) => handleChange(e.target.value)}
            disabled={loading}
            className="flex-1 px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          >
            <option value="">System Default</option>
            {devices.map((d) => (
              <option key={d.deviceId} value={d.deviceId}>
                {d.label}
              </option>
            ))}
          </select>
          <button
            onClick={enumerateDevices}
            disabled={loading}
            title="Refresh microphone list"
            className="text-xs px-2 py-1 text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)] rounded transition-colors disabled:opacity-50"
          >
            {loading ? "..." : "Refresh"}
          </button>
        </div>
      )}
    </div>
  );
}
