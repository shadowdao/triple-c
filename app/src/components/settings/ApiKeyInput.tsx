export default function ApiKeyInput() {
  return (
    <div>
      <label className="block text-sm font-medium mb-1">Backend</label>
      <p className="text-xs text-[var(--text-secondary)] mb-3">
        Each project can use <strong>claude login</strong> (OAuth, run inside the terminal) or <strong>AWS Bedrock</strong>. Set backend per-project.
      </p>
    </div>
  );
}
