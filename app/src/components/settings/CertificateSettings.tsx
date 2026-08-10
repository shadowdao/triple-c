import { useEffect, useState } from "react";
import { useSettings } from "../../hooks/useSettings";
import CaCertPathInput from "./CaCertPathInput";

const INPUT_CLASS =
  "flex-1 min-w-0 px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]";

/**
 * Global corporate CA certificate setting.
 *
 * Applies to every project unless one overrides it in Project Home → Config →
 * Access. Changing it recreates each container on its next start — the
 * certificate is copied into the container's trust store once, at start, so
 * there is nowhere else for a change to land.
 */
export default function CertificateSettings() {
  const { appSettings, saveSettings } = useSettings();
  const [path, setPath] = useState(appSettings?.ca_cert_path ?? "");

  useEffect(() => {
    setPath(appSettings?.ca_cert_path ?? "");
  }, [appSettings?.ca_cert_path]);

  const commit = async (value: string) => {
    if (!appSettings) return;
    const next = value.trim() || null;
    if (next === appSettings.ca_cert_path) return;
    await saveSettings({ ...appSettings, ca_cert_path: next });
  };

  return (
    <div>
      <label
        className="block text-sm font-medium mb-1"
        htmlFor="global-ca-cert-path"
      >
        Corporate CA Certificate
      </label>
      <p className="text-xs text-[var(--text-secondary)] mb-1.5">
        A certificate file, or a folder of them, for organisations whose network
        inspects TLS. Mounted read-only into every container and trusted by
        curl, git, npm, pip, Chromium and Claude Code itself. Per-project
        settings override this; changing it recreates containers on next start.
      </p>
      <CaCertPathInput
        id="global-ca-cert-path"
        value={path}
        onChange={setPath}
        onCommit={commit}
        placeholder="/etc/ssl/certs/corp-root.pem"
        emptyHint="Not set — containers trust only the public CAs shipped with the image."
        inputClassName={INPUT_CLASS}
      />
    </div>
  );
}
