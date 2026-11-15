import { useState } from "react";
import { awsPolicyDocument } from "../../../lib/tauri-commands";
import { awsErrorMessage, toAwsError } from "../../../lib/aws-error";
import type { PolicyFile, Tier } from "../../../lib/types";
import Button from "../../ui/Button";

/** A tier's policy files, loaded on demand: each one shown and copied on its own. */
export default function PolicyFiles({
  tier,
  label,
  onCopy,
}: {
  tier: Tier;
  label: string;
  onCopy: (file: PolicyFile) => void;
}) {
  const [files, setFiles] = useState<PolicyFile[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setError(null);
    try {
      setFiles(await awsPolicyDocument(tier));
    } catch (e) {
      setError(`Could not load the policy: ${awsErrorMessage(toAwsError(e))}`);
    }
  };

  if (!files) {
    return (
      <div className="space-y-1">
        <Button size="sm" onClick={load}>
          {label}
        </Button>
        {error && (
          <p role="alert" className="text-xs text-[var(--error)]">
            {error}
          </p>
        )}
      </div>
    );
  }
  return (
    <div className="space-y-1 text-xs">
      <p className="text-[var(--text-secondary)]">
        {tier === "workspace_user" ? "Workspace user" : "Environment admin"}:{" "}
        {files.length === 1
          ? "attach this file as a customer managed policy."
          : `Attach all ${files.length} files, each as its own customer managed policy.`}
      </p>
      {files.map((f) => (
        <div key={f.name} className="space-y-1">
          <div className="flex items-center gap-2">
            <span className="font-mono text-[var(--text-primary)]">{f.name}</span>
            <Button size="sm" aria-label={`Copy ${f.name}`} onClick={() => onCopy(f)}>
              Copy
            </Button>
          </div>
          <details>
            <summary className="cursor-pointer text-[var(--text-secondary)]">Show JSON</summary>
            <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-all text-[var(--text-secondary)]">
              {f.json}
            </pre>
          </details>
        </div>
      ))}
    </div>
  );
}

