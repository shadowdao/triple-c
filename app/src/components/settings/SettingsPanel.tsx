import ApiKeyInput from "./ApiKeyInput";
import DockerSettings from "./DockerSettings";
import AwsSettings from "./AwsSettings";

export default function SettingsPanel() {
  return (
    <div className="p-4 space-y-6">
      <h2 className="text-xs font-semibold uppercase text-[var(--text-secondary)]">
        Settings
      </h2>
      <ApiKeyInput />
      <DockerSettings />
      <AwsSettings />
    </div>
  );
}
