import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Project, AuthMode, BedrockConfig, BedrockAuthMethod } from "../../lib/types";
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

  const defaultBedrockConfig: BedrockConfig = {
    auth_method: "static_credentials",
    aws_region: "us-east-1",
    aws_access_key_id: null,
    aws_secret_access_key: null,
    aws_session_token: null,
    aws_profile: null,
    aws_bearer_token: null,
    model_id: null,
    disable_prompt_caching: false,
  };

  const handleAuthModeChange = async (mode: AuthMode) => {
    try {
      const updates: Partial<Project> = { auth_mode: mode };
      if (mode === "bedrock" && !project.bedrock_config) {
        updates.bedrock_config = defaultBedrockConfig;
      }
      await update({ ...project, ...updates });
    } catch (e) {
      setError(String(e));
    }
  };

  const updateBedrockConfig = async (patch: Partial<BedrockConfig>) => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, ...patch } });
    } catch {}
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
            <button
              onClick={(e) => { e.stopPropagation(); handleAuthModeChange("bedrock"); }}
              disabled={!isStopped}
              className={`px-2 py-0.5 rounded transition-colors ${
                project.auth_mode === "bedrock"
                  ? "bg-[var(--accent)] text-white"
                  : "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)]"
              } disabled:opacity-50`}
            >
              Bedrock
            </button>
          </div>

          {/* Action buttons */}
          <div className="flex items-center gap-1 flex-wrap">
            {isStopped ? (
              <>
                <ActionButton onClick={handleStart} disabled={loading} label="Start" />
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
            ) : project.status === "running" ? (
              <>
                <ActionButton onClick={handleStop} disabled={loading} label="Stop" />
                <ActionButton onClick={handleOpenTerminal} disabled={loading} label="Terminal" accent />
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

              {/* Environment Variables */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Environment Variables</label>
                {(project.custom_env_vars ?? []).map((ev, i) => (
                  <div key={i} className="flex gap-1 mb-1">
                    <input
                      value={ev.key}
                      onChange={async (e) => {
                        const vars = [...(project.custom_env_vars ?? [])];
                        vars[i] = { ...vars[i], key: e.target.value };
                        try { await update({ ...project, custom_env_vars: vars }); } catch {}
                      }}
                      placeholder="KEY"
                      disabled={!isStopped}
                      className="w-1/3 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
                    />
                    <input
                      value={ev.value}
                      onChange={async (e) => {
                        const vars = [...(project.custom_env_vars ?? [])];
                        vars[i] = { ...vars[i], value: e.target.value };
                        try { await update({ ...project, custom_env_vars: vars }); } catch {}
                      }}
                      placeholder="value"
                      disabled={!isStopped}
                      className="flex-1 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
                    />
                    <button
                      onClick={async () => {
                        const vars = (project.custom_env_vars ?? []).filter((_, j) => j !== i);
                        try { await update({ ...project, custom_env_vars: vars }); } catch {}
                      }}
                      disabled={!isStopped}
                      className="px-1.5 py-1 text-xs text-[var(--error)] hover:bg-[var(--bg-primary)] rounded disabled:opacity-50 transition-colors"
                    >
                      x
                    </button>
                  </div>
                ))}
                <button
                  onClick={async () => {
                    const vars = [...(project.custom_env_vars ?? []), { key: "", value: "" }];
                    try { await update({ ...project, custom_env_vars: vars }); } catch {}
                  }}
                  disabled={!isStopped}
                  className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] disabled:opacity-50 transition-colors"
                >
                  + Add variable
                </button>
              </div>

              {/* Claude Instructions */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Claude Instructions</label>
                <textarea
                  value={project.claude_instructions ?? ""}
                  onChange={async (e) => {
                    try { await update({ ...project, claude_instructions: e.target.value || null }); } catch {}
                  }}
                  placeholder="Per-project instructions for Claude Code (written to ~/.claude/CLAUDE.md in container)"
                  disabled={!isStopped}
                  rows={3}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 resize-y font-mono"
                />
              </div>

              {/* Bedrock config */}
              {project.auth_mode === "bedrock" && (() => {
                const bc = project.bedrock_config ?? defaultBedrockConfig;
                const inputCls = "w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50";
                return (
                  <div className="space-y-2 pt-1 border-t border-[var(--border-color)]">
                    <label className="block text-xs font-medium text-[var(--text-primary)]">AWS Bedrock</label>

                    {/* Sub-method selector */}
                    <div className="flex items-center gap-1 text-xs">
                      <span className="text-[var(--text-secondary)] mr-1">Method:</span>
                      {(["static_credentials", "profile", "bearer_token"] as BedrockAuthMethod[]).map((m) => (
                        <button
                          key={m}
                          onClick={() => updateBedrockConfig({ auth_method: m })}
                          disabled={!isStopped}
                          className={`px-2 py-0.5 rounded transition-colors ${
                            bc.auth_method === m
                              ? "bg-[var(--accent)] text-white"
                              : "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)]"
                          } disabled:opacity-50`}
                        >
                          {m === "static_credentials" ? "Keys" : m === "profile" ? "Profile" : "Token"}
                        </button>
                      ))}
                    </div>

                    {/* AWS Region (always shown) */}
                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">AWS Region</label>
                      <input
                        value={bc.aws_region}
                        onChange={(e) => updateBedrockConfig({ aws_region: e.target.value })}
                        placeholder="us-east-1"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                    </div>

                    {/* Static credentials fields */}
                    {bc.auth_method === "static_credentials" && (
                      <>
                        <div>
                          <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Access Key ID</label>
                          <input
                            value={bc.aws_access_key_id ?? ""}
                            onChange={(e) => updateBedrockConfig({ aws_access_key_id: e.target.value || null })}
                            placeholder="AKIA..."
                            disabled={!isStopped}
                            className={inputCls}
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Secret Access Key</label>
                          <input
                            type="password"
                            value={bc.aws_secret_access_key ?? ""}
                            onChange={(e) => updateBedrockConfig({ aws_secret_access_key: e.target.value || null })}
                            disabled={!isStopped}
                            className={inputCls}
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Session Token (optional)</label>
                          <input
                            type="password"
                            value={bc.aws_session_token ?? ""}
                            onChange={(e) => updateBedrockConfig({ aws_session_token: e.target.value || null })}
                            disabled={!isStopped}
                            className={inputCls}
                          />
                        </div>
                      </>
                    )}

                    {/* Profile field */}
                    {bc.auth_method === "profile" && (
                      <div>
                        <label className="block text-xs text-[var(--text-secondary)] mb-0.5">AWS Profile</label>
                        <input
                          value={bc.aws_profile ?? ""}
                          onChange={(e) => updateBedrockConfig({ aws_profile: e.target.value || null })}
                          placeholder="default"
                          disabled={!isStopped}
                          className={inputCls}
                        />
                      </div>
                    )}

                    {/* Bearer token field */}
                    {bc.auth_method === "bearer_token" && (
                      <div>
                        <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Bearer Token</label>
                        <input
                          type="password"
                          value={bc.aws_bearer_token ?? ""}
                          onChange={(e) => updateBedrockConfig({ aws_bearer_token: e.target.value || null })}
                          disabled={!isStopped}
                          className={inputCls}
                        />
                      </div>
                    )}

                    {/* Model override */}
                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Model ID (optional)</label>
                      <input
                        value={bc.model_id ?? ""}
                        onChange={(e) => updateBedrockConfig({ model_id: e.target.value || null })}
                        placeholder="anthropic.claude-sonnet-4-20250514-v1:0"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                    </div>
                  </div>
                );
              })()}
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
