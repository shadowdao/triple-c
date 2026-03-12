import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import TerminalTabs from "../terminal/TerminalTabs";
import { useAppState } from "../../store/appState";
import { useSettings } from "../../hooks/useSettings";
import UpdateDialog from "../settings/UpdateDialog";
import ImageUpdateDialog from "../settings/ImageUpdateDialog";
import HelpDialog from "./HelpDialog";

export default function TopBar() {
  const { dockerAvailable, imageExists, updateInfo, imageUpdateInfo, appVersion, setUpdateInfo, setImageUpdateInfo } = useAppState(
    useShallow(s => ({
      dockerAvailable: s.dockerAvailable,
      imageExists: s.imageExists,
      updateInfo: s.updateInfo,
      imageUpdateInfo: s.imageUpdateInfo,
      appVersion: s.appVersion,
      setUpdateInfo: s.setUpdateInfo,
      setImageUpdateInfo: s.setImageUpdateInfo,
    }))
  );
  const { appSettings, saveSettings } = useSettings();
  const [showUpdateDialog, setShowUpdateDialog] = useState(false);
  const [showImageUpdateDialog, setShowImageUpdateDialog] = useState(false);
  const [showHelpDialog, setShowHelpDialog] = useState(false);

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

  const handleImageUpdateDismiss = async () => {
    if (appSettings && imageUpdateInfo) {
      await saveSettings({
        ...appSettings,
        dismissed_image_digest: imageUpdateInfo.remote_digest,
      });
    }
    setImageUpdateInfo(null);
    setShowImageUpdateDialog(false);
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
          {imageUpdateInfo && (
            <button
              onClick={() => setShowImageUpdateDialog(true)}
              className="px-2 py-0.5 rounded text-xs font-medium bg-[var(--warning,#f59e0b)] text-white hover:opacity-80 transition-colors"
              title="A newer container image is available"
            >
              Image Update
            </button>
          )}
          <StatusDot ok={dockerAvailable === true} label="Docker" />
          <StatusDot ok={imageExists === true} label="Image" />
          <button
            onClick={() => setShowHelpDialog(true)}
            title="Help"
            className="ml-1 w-5 h-5 flex items-center justify-center rounded-full border border-[var(--border-color)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:border-[var(--text-secondary)] transition-colors text-xs font-semibold leading-none"
          >
            ?
          </button>
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
      {showImageUpdateDialog && imageUpdateInfo && (
        <ImageUpdateDialog
          imageUpdateInfo={imageUpdateInfo}
          onDismiss={handleImageUpdateDismiss}
          onClose={() => setShowImageUpdateDialog(false)}
        />
      )}
      {showHelpDialog && (
        <HelpDialog onClose={() => setShowHelpDialog(false)} />
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
