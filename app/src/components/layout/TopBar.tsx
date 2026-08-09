import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import MainTabs from "./MainTabs";
import { useAppState } from "../../store/appState";
import { useSettings } from "../../hooks/useSettings";
import UpdateDialog from "../settings/UpdateDialog";
import ImageUpdateDialog from "../settings/ImageUpdateDialog";
import HelpDialog from "./HelpDialog";
import StatusIndicator, { type StatusTone } from "../ui/StatusIndicator";

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
      <div className="flex items-center h-10 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-panel)] overflow-hidden">
        <div className="flex-1 overflow-x-auto pl-1">
          <MainTabs />
        </div>
        <div className="flex items-center gap-3 px-3 flex-shrink-0 text-xs text-[var(--text-secondary)]">
          {updateInfo && (
            <button
              type="button"
              onClick={() => setShowUpdateDialog(true)}
              className="h-6 px-2 rounded-[var(--radius-control)] text-xs font-medium bg-[var(--accent-emphasis)] text-white hover:bg-[var(--accent-emphasis-hover)] transition-colors"
            >
              Update
            </button>
          )}
          {imageUpdateInfo && (
            <button
              type="button"
              onClick={() => setShowImageUpdateDialog(true)}
              className="h-6 px-2 rounded-[var(--radius-control)] text-xs font-medium bg-[var(--warning-emphasis)] text-white hover:opacity-90 transition-colors"
              title="A newer container image is available"
            >
              Image Update
            </button>
          )}
          <HealthDot
            state={dockerAvailable}
            okLabel="Docker"
            failLabel="Docker unavailable"
            pendingLabel="Docker — checking"
          />
          <HealthDot
            state={imageExists}
            okLabel="Image"
            failLabel="Image missing"
            pendingLabel="Image — checking"
          />
          <button
            type="button"
            onClick={() => setShowHelpDialog(true)}
            title="Help"
            aria-label="Help"
            className="w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] border border-[var(--border-color)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:border-[var(--text-secondary)] transition-colors text-xs font-semibold leading-none"
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

/**
 * `null` (still checking) is visually distinct and pulses; `false` is an
 * outage and renders red — previously both fell through to the same gray dot.
 */
function HealthDot({
  state,
  okLabel,
  failLabel,
  pendingLabel,
}: {
  state: boolean | null;
  okLabel: string;
  failLabel: string;
  pendingLabel: string;
}) {
  let tone: StatusTone = "unknown";
  let label = pendingLabel;
  if (state === true) {
    tone = "ok";
    label = okLabel;
  } else if (state === false) {
    tone = "error";
    label = failLabel;
  }
  return <StatusIndicator tone={tone} label={label} />;
}
