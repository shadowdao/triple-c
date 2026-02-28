import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import TerminalTabs from "../terminal/TerminalTabs";
import { useAppState } from "../../store/appState";
import { useSettings } from "../../hooks/useSettings";
import UpdateDialog from "../settings/UpdateDialog";

export default function TopBar() {
  const { dockerAvailable, imageExists, updateInfo, appVersion, setUpdateInfo } = useAppState(
    useShallow(s => ({
      dockerAvailable: s.dockerAvailable,
      imageExists: s.imageExists,
      updateInfo: s.updateInfo,
      appVersion: s.appVersion,
      setUpdateInfo: s.setUpdateInfo,
    }))
  );
  const { appSettings, saveSettings } = useSettings();
  const [showUpdateDialog, setShowUpdateDialog] = useState(false);

  const handleDismiss = async () => {
    if (appSettings && updateInfo) {
      await saveSettings({
        ...appSettings,
        dismissed_update_version: updateInfo.version,
      });
    }
    setUpdateInfo(null);
    setShowUpdateDialog(false);
  };

  return (
    <>
      <div className="flex items-center h-10 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg overflow-hidden">
        <div className="flex-1 overflow-x-auto pl-2">
          <TerminalTabs />
        </div>
        <div className="flex items-center gap-2 px-4 flex-shrink-0 text-xs text-[var(--text-secondary)]">
          {updateInfo && (
            <button
              onClick={() => setShowUpdateDialog(true)}
              className="px-2 py-0.5 rounded text-xs font-medium bg-[var(--accent)] text-white animate-pulse hover:bg-[var(--accent-hover)] transition-colors"
            >
              Update
            </button>
          )}
          <StatusDot ok={dockerAvailable === true} label="Docker" />
          <StatusDot ok={imageExists === true} label="Image" />
        </div>
      </div>
      {showUpdateDialog && updateInfo && (
        <UpdateDialog
          updateInfo={updateInfo}
          currentVersion={appVersion}
          onDismiss={handleDismiss}
          onClose={() => setShowUpdateDialog(false)}
        />
      )}
    </>
  );
}

function StatusDot({ ok, label }: { ok: boolean; label: string }) {
  return (
    <span className="flex items-center gap-1">
      <span
        className={`inline-block w-2 h-2 rounded-full ${
          ok ? "bg-[var(--success)]" : "bg-[var(--text-secondary)]"
        }`}
      />
      {label}
    </span>
  );
}
