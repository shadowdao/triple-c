import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Project, AuthMode } from "../../lib/types";
import { useProjects } from "../../hooks/useProjects";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState } from "../../store/appState";

interface Props {
  project: Project;
}

export default function ProjectCard({ project }: Props) {
  const { selectedProjectId, setSelectedProject } = useAppState();
  const { start, stop, rebuild, remove, update } = useProjects();
  const { open: openTerminal } = useTerminal();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showConfig, setShowConfig] = useState(false);
  const isSelected = selectedProjectId === project.id;
  const isStopped = project.status === "stopped" || project.status === "error";

  const handleStart = async () => {
    setLoading(true);
    setError(null);
    try {
      await start(project.id);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleStop = async () => {
    setLoading(true);
    setError(null);
    try {
      await stop(project.id);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleOpenTerminal = async () => {
    try {
      await openTerminal(project.id, project.name);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleAuthModeChange = async (mode: AuthMode) => {
    try {
      await update({ ...project, auth_mode: mode });
    } catch (e) {
      setError(String(e));
    }
  };

  const handleBrowseSSH = async () => {
    const selected = await open({ directory: true, multiple: false });
    if (selected) {
      try {
        await update({ ...project, ssh_key_path: selected as string });
      } catch (e) {
        setError(String(e));
      }
    }
  };

  const statusColor = {
    stopped: "bg-[var(--text-secondary)]",
    starting: "bg-[var(--warning)]",
    running: "bg-[var(--success)]",
    stopping: "bg-[var(--warning)]",
    error: "bg-[var(--error)]",
  }[project.status];

  return (
    <div
      onClick={() => setSelectedProject(project.id)}
      className={`px-3 py-2 rounded cursor-pointer transition-colors ${
        isSelected
          ? "bg-[var(--bg-tertiary)]"
          : "hover:bg-[var(--bg-tertiary)]"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className={`w-2 h-2 rounded-full flex-shrink-0 ${statusColor}`} />
        <span className="text-sm font-medium truncate flex-1">{project.name}</span>
      </div>
      <div className="text-xs text-[var(--text-secondary)] truncate mt-0.5 ml-4">
        {project.path}
      </div>

      {isSelected && (
        <div className="mt-2 ml-4 space-y-2">
          {/* Auth mode selector */}
          <div className="flex items-center gap-1 text-xs">
            <span className="text-[var(--text-secondary)] mr-1">Auth:</span>
            <button
              onClick={(e) => { e.stopPropagation(); handleAuthModeChange("login"); }}
              disabled={!isStopped}
              className={`px-2 py-0.5 rounded transition-colors ${
                project.auth_mode === "login"
                  ? "bg-[var(--accent)] text-white"
                  : "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)]"
              } disabled:opacity-50`}
            >
              /login
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); handleAuthModeChange("api_key"); }}
              disabled={!isStopped}
              className={`px-2 py-0.5 rounded transition-colors ${
                project.auth_mode === "api_key"
                  ? "bg-[var(--accent)] text-white"
                  : "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)]"
              } disabled:opacity-50`}
            >
              API key
            </button>
          </div>

          {/* Action buttons */}
          <div className="flex items-center gap-1">
            {isStopped ? (
              <ActionButton onClick={handleStart} disabled={loading} label="Start" />
            ) : project.status === "running" ? (
              <>
                <ActionButton onClick={handleStop} disabled={loading} label="Stop" />
                <ActionButton onClick={handleOpenTerminal} disabled={loading} label="Terminal" accent />
                <ActionButton
                  onClick={async () => {
                    setLoading(true);
                    try { await rebuild(project.id); } catch (e) { setError(String(e)); }
                    setLoading(false);
                  }}
                  disabled={loading}
                  label="Reset"
                />
              </>
            ) : (
              <span className="text-xs text-[var(--text-secondary)]">
                {project.status}...
              </span>
            )}
            <ActionButton
              onClick={(e) => { e?.stopPropagation?.(); setShowConfig(!showConfig); }}
              disabled={false}
              label={showConfig ? "Hide" : "Config"}
            />
            <ActionButton
              onClick={async () => {
                if (confirm(`Remove project "${project.name}"?`)) {
                  await remove(project.id);
                }
              }}
              disabled={loading}
              label="Remove"
              danger
            />
          </div>

          {/* Config panel */}
          {showConfig && (
            <div className="space-y-2 pt-1 border-t border-[var(--border-color)]" onClick={(e) => e.stopPropagation()}>
              {/* SSH Key */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">SSH Key Directory</label>
                <div className="flex gap-1">
                  <input
                    value={project.ssh_key_path ?? ""}
                    onChange={async (e) => {
                      try { await update({ ...project, ssh_key_path: e.target.value || null }); } catch {}
                    }}
                    placeholder="~/.ssh"
                    disabled={!isStopped}
                    className="flex-1 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                  />
                  <button
                    onClick={handleBrowseSSH}
                    disabled={!isStopped}
                    className="px-2 py-1 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] disabled:opacity-50 transition-colors"
                  >
                    ...
                  </button>
                </div>
              </div>

              {/* Git Name */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Git Name</label>
                <input
                  value={project.git_user_name ?? ""}
                  onChange={async (e) => {
                    try { await update({ ...project, git_user_name: e.target.value || null }); } catch {}
                  }}
                  placeholder="Your Name"
                  disabled={!isStopped}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                />
              </div>

              {/* Git Email */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Git Email</label>
                <input
                  value={project.git_user_email ?? ""}
                  onChange={async (e) => {
                    try { await update({ ...project, git_user_email: e.target.value || null }); } catch {}
                  }}
                  placeholder="you@example.com"
                  disabled={!isStopped}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                />
              </div>

              {/* Git Token (HTTPS) */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Git HTTPS Token</label>
                <input
                  type="password"
                  value={project.git_token ?? ""}
                  onChange={async (e) => {
                    try { await update({ ...project, git_token: e.target.value || null }); } catch {}
                  }}
                  placeholder="ghp_..."
                  disabled={!isStopped}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                />
              </div>

              {/* Docker access toggle */}
              <div className="flex items-center gap-2">
                <label className="text-xs text-[var(--text-secondary)]">Allow container spawning</label>
                <button
                  onClick={async () => {
                    try { await update({ ...project, allow_docker_access: !project.allow_docker_access }); } catch {}
                  }}
                  disabled={!isStopped}
                  className={`px-2 py-0.5 text-xs rounded transition-colors disabled:opacity-50 ${
                    project.allow_docker_access
                      ? "bg-[var(--success)] text-white"
                      : "bg-[var(--bg-primary)] border border-[var(--border-color)] text-[var(--text-secondary)]"
                  }`}
                >
                  {project.allow_docker_access ? "ON" : "OFF"}
                </button>
              </div>
            </div>
          )}
        </div>
      )}

      {error && (
        <div className="text-xs text-[var(--error)] mt-1 ml-4">{error}</div>
      )}
    </div>
  );
}

function ActionButton({
  onClick,
  disabled,
  label,
  accent,
  danger,
}: {
  onClick: (e?: React.MouseEvent) => void;
  disabled: boolean;
  label: string;
  accent?: boolean;
  danger?: boolean;
}) {
  let color = "text-[var(--text-secondary)] hover:text-[var(--text-primary)]";
  if (accent) color = "text-[var(--accent)] hover:text-[var(--accent-hover)]";
  if (danger) color = "text-[var(--error)] hover:text-[var(--error)]";

  return (
    <button
      onClick={(e) => { e.stopPropagation(); onClick(e); }}
      disabled={disabled}
      className={`text-xs px-2 py-0.5 rounded transition-colors disabled:opacity-50 ${color} hover:bg-[var(--bg-primary)]`}
    >
      {label}
    </button>
  );
}
