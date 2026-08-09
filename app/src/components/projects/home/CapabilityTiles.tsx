import { useEffect, useState } from "react";
import type {
  CapabilityGroup,
  ContainerCapabilities,
  Project,
} from "../../../lib/types";
import { listContainerCapabilities } from "../../../lib/tauri-commands";
import Modal from "../../ui/Modal";
import Button from "../../ui/Button";

/**
 * Read-only inventory of what Claude Code can do inside this container.
 * Triple-C surfaces counts and launches the real editors in the terminal —
 * it does not rebuild `/agents`, `/hooks`, or `/plugins` as forms.
 */
const GROUPS: { key: keyof ContainerCapabilities; label: string }[] = [
  { key: "skills", label: "Skills" },
  { key: "agents", label: "Agents" },
  { key: "commands", label: "Commands" },
  { key: "hooks", label: "Hooks" },
  { key: "plugins", label: "Plugins" },
  { key: "mcp_servers", label: "MCP servers" },
];

const SLASH_HINT: Partial<Record<keyof ContainerCapabilities, string>> = {
  agents: "/agents",
  hooks: "/hooks",
  plugins: "/plugins",
  mcp_servers: "/mcp",
};

interface Props {
  project: Project;
  onManageInTerminal: (command: string) => void;
}

export default function CapabilityTiles({ project, onManageInTerminal }: Props) {
  const [capabilities, setCapabilities] = useState<ContainerCapabilities | null>(null);
  const [loading, setLoading] = useState(false);
  const [open, setOpen] = useState<keyof ContainerCapabilities | null>(null);

  const running = project.status === "running";

  useEffect(() => {
    if (!running) {
      setCapabilities(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    listContainerCapabilities(project.id)
      .then((c) => {
        if (!cancelled) setCapabilities(c);
      })
      // Introspection degrades to "nothing found" when the container is
      // unreachable — that is an empty state, not an error banner.
      .catch(() => {
        if (!cancelled) setCapabilities(null);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [project.id, running, project.container_id]);

  const openGroup: CapabilityGroup | null =
    open && capabilities ? capabilities[open] : null;
  const openLabel = GROUPS.find((g) => g.key === open)?.label ?? "";

  return (
    <section>
      <h2 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)] mb-2">
        Capabilities
      </h2>

      {!running ? (
        <p className="text-xs text-[var(--text-secondary)]">
          Start the container to read its skills, agents, commands, hooks and plugins.
        </p>
      ) : loading && !capabilities ? (
        <p className="text-xs text-[var(--text-secondary)]">Reading container volume…</p>
      ) : (
        <div className="flex flex-wrap gap-2">
          {GROUPS.map(({ key, label }) => {
            const count = capabilities?.[key].count ?? 0;
            return (
              <button
                key={key}
                type="button"
                disabled={count === 0}
                onClick={() => setOpen(key)}
                className="flex items-baseline gap-2 px-3 py-2 min-w-[7.5rem] text-left bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-panel)] hover:border-[var(--accent)] disabled:hover:border-[var(--border-color)] disabled:cursor-default transition-colors"
              >
                <span
                  className={`text-lg font-semibold tabular-nums ${
                    count === 0 ? "text-[var(--text-disabled)]" : "text-[var(--text-primary)]"
                  }`}
                >
                  {count}
                </span>
                <span className="text-xs text-[var(--text-secondary)]">{label}</span>
              </button>
            );
          })}
        </div>
      )}

      {open && openGroup && (
        <Modal
          title={`${openLabel} — ${project.name}`}
          description={
            SLASH_HINT[open]
              ? `Claude Code manages these with ${SLASH_HINT[open]}.`
              : undefined
          }
          onClose={() => setOpen(null)}
          widthClassName="w-[34rem]"
          footer={
            <>
              <Button
                variant="primary"
                onClick={() => {
                  setOpen(null);
                  // Claude Code owns the editors; we just deep-link into them.
                  onManageInTerminal("claude");
                }}
              >
                Manage in terminal
              </Button>
              <Button onClick={() => setOpen(null)}>Close</Button>
            </>
          }
        >
          {openGroup.items.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">Nothing configured.</p>
          ) : (
            <ul className="space-y-2">
              {openGroup.items.map((item, i) => (
                <li
                  key={`${item.name}-${i}`}
                  className="pb-2 border-b border-[var(--border-color)] last:border-b-0"
                >
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-[var(--text-primary)] font-mono">
                      {item.name}
                    </span>
                    <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded-[var(--radius-control)] bg-[var(--accent-muted)] text-[var(--accent)]">
                      {item.scope}
                    </span>
                  </div>
                  {item.description && (
                    <p className="mt-0.5 text-xs text-[var(--text-secondary)]">
                      {item.description}
                    </p>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Modal>
      )}
    </section>
  );
}
