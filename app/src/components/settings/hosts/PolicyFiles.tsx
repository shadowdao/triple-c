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
      <div className="space-y-1 min-w-0">
        <Button size="sm" wrap onClick={load}>
          {label}
        </Button>
        {error && (
          <p role="alert" className="text-xs text-[var(--error)] break-words">
            {error}
          </p>
        )}
      </div>
    );
  }
  return (
    <div className="space-y-1.5 text-xs min-w-0">
      <p className="text-[var(--text-secondary)] leading-snug break-words">
        {tier === "workspace_user" ? "Workspace user" : "Environment admin"}:{" "}
        {files.length === 1
          ? "attach this file as a customer managed policy."
          : `Attach all ${files.length} files, each as its own customer managed policy.`}
      </p>
      {/* One copy button per file, in a grid that drops to one column when narrow. */}
      <div data-testid="policy-copy-grid" className="grid gap-1.5 grid-cols-[repeat(auto-fill,minmax(9rem,1fr))]">
        {files.map((f) => (
          <Button
            key={f.name}
            size="sm"
            wrap
            className="w-full justify-start break-all"
            aria-label={`Copy ${f.name}`}
            onClick={() => onCopy(f)}
          >
            Copy <span className="font-mono">{f.name}</span>
          </Button>
        ))}
      </div>
      {files.map((f) => (
        <details key={f.name} className="min-w-0">
          <summary className="cursor-pointer text-[var(--text-secondary)] break-all">
            Show JSON of <span className="font-mono">{f.name}</span>
          </summary>
          <pre className="mt-1 max-h-48 max-w-full overflow-auto whitespace-pre-wrap break-all text-[var(--text-secondary)]">
            {f.json}
          </pre>
        </details>
      ))}
    </div>
  );
}
