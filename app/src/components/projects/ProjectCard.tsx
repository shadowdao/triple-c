import { useState, useEffect } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import type { Project, ProjectPath, Backend, BedrockConfig, BedrockAuthMethod, OllamaConfig, LiteLlmConfig } from "../../lib/types";
import { useProjects } from "../../hooks/useProjects";
import { useMcpServers } from "../../hooks/useMcpServers";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState } from "../../store/appState";
import EnvVarsModal from "./EnvVarsModal";
import PortMappingsModal from "./PortMappingsModal";
import ClaudeInstructionsModal from "./ClaudeInstructionsModal";
import ContainerProgressModal from "./ContainerProgressModal";
import FileManagerModal from "./FileManagerModal";
import ConfirmRemoveModal from "./ConfirmRemoveModal";
import Tooltip from "../ui/Tooltip";

interface Props {
  project: Project;
}

export default function ProjectCard({ project }: Props) {
  const selectedProjectId = useAppState(s => s.selectedProjectId);
  const setSelectedProject = useAppState(s => s.setSelectedProject);
  const { start, stop, rebuild, remove, update } = useProjects();
  const { mcpServers } = useMcpServers();
  const { open: openTerminal } = useTerminal();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showConfig, setShowConfig] = useState(false);
  const [showEnvVarsModal, setShowEnvVarsModal] = useState(false);
  const [showPortMappingsModal, setShowPortMappingsModal] = useState(false);
  const [showClaudeInstructionsModal, setShowClaudeInstructionsModal] = useState(false);
  const [showFileManager, setShowFileManager] = useState(false);
  const [progressMsg, setProgressMsg] = useState<string | null>(null);
  const [activeOperation, setActiveOperation] = useState<"starting" | "stopping" | "resetting" | null>(null);
  const [operationCompleted, setOperationCompleted] = useState(false);
  const [showRemoveModal, setShowRemoveModal] = useState(false);
  const [isEditingName, setIsEditingName] = useState(false);
  const [editName, setEditName] = useState(project.name);
  const isSelected = selectedProjectId === project.id;
  const isStopped = project.status === "stopped" || project.status === "error";

  // Local state for text fields (save on blur, not on every keystroke)
  const [paths, setPaths] = useState<ProjectPath[]>(project.paths ?? []);
  const [sshKeyPath, setSshKeyPath] = useState(project.ssh_key_path ?? "");
  const [gitName, setGitName] = useState(project.git_user_name ?? "");
  const [gitEmail, setGitEmail] = useState(project.git_user_email ?? "");
  const [gitToken, setGitToken] = useState(project.git_token ?? "");
  const [claudeInstructions, setClaudeInstructions] = useState(project.claude_instructions ?? "");
  const [envVars, setEnvVars] = useState(project.custom_env_vars ?? []);
  const [portMappings, setPortMappings] = useState(project.port_mappings ?? []);

  // Bedrock local state for text fields
  const [bedrockRegion, setBedrockRegion] = useState(project.bedrock_config?.aws_region ?? "us-east-1");
  const [bedrockAccessKeyId, setBedrockAccessKeyId] = useState(project.bedrock_config?.aws_access_key_id ?? "");
  const [bedrockSecretKey, setBedrockSecretKey] = useState(project.bedrock_config?.aws_secret_access_key ?? "");
  const [bedrockSessionToken, setBedrockSessionToken] = useState(project.bedrock_config?.aws_session_token ?? "");
  const [bedrockProfile, setBedrockProfile] = useState(project.bedrock_config?.aws_profile ?? "");
  const [bedrockBearerToken, setBedrockBearerToken] = useState(project.bedrock_config?.aws_bearer_token ?? "");
  const [bedrockModelId, setBedrockModelId] = useState(project.bedrock_config?.model_id ?? "");

  // Ollama local state
  const [ollamaBaseUrl, setOllamaBaseUrl] = useState(project.ollama_config?.base_url ?? "http://host.docker.internal:11434");
  const [ollamaModelId, setOllamaModelId] = useState(project.ollama_config?.model_id ?? "");

  // LiteLLM local state
  const [litellmBaseUrl, setLitellmBaseUrl] = useState(project.litellm_config?.base_url ?? "http://host.docker.internal:4000");
  const [litellmApiKey, setLitellmApiKey] = useState(project.litellm_config?.api_key ?? "");
  const [litellmModelId, setLitellmModelId] = useState(project.litellm_config?.model_id ?? "");

  // Sync local state when project prop changes (e.g., after save or external update)
  useEffect(() => {
    setEditName(project.name);
    setPaths(project.paths ?? []);
    setSshKeyPath(project.ssh_key_path ?? "");
    setGitName(project.git_user_name ?? "");
    setGitEmail(project.git_user_email ?? "");
    setGitToken(project.git_token ?? "");
    setClaudeInstructions(project.claude_instructions ?? "");
    setEnvVars(project.custom_env_vars ?? []);
    setPortMappings(project.port_mappings ?? []);
    setBedrockRegion(project.bedrock_config?.aws_region ?? "us-east-1");
    setBedrockAccessKeyId(project.bedrock_config?.aws_access_key_id ?? "");
    setBedrockSecretKey(project.bedrock_config?.aws_secret_access_key ?? "");
    setBedrockSessionToken(project.bedrock_config?.aws_session_token ?? "");
    setBedrockProfile(project.bedrock_config?.aws_profile ?? "");
    setBedrockBearerToken(project.bedrock_config?.aws_bearer_token ?? "");
    setBedrockModelId(project.bedrock_config?.model_id ?? "");
    setOllamaBaseUrl(project.ollama_config?.base_url ?? "http://host.docker.internal:11434");
    setOllamaModelId(project.ollama_config?.model_id ?? "");
    setLitellmBaseUrl(project.litellm_config?.base_url ?? "http://host.docker.internal:4000");
    setLitellmApiKey(project.litellm_config?.api_key ?? "");
    setLitellmModelId(project.litellm_config?.model_id ?? "");
  }, [project]);

  // Listen for container progress events
  useEffect(() => {
    const unlisten = listen<{ project_id: string; message: string }>(
      "container-progress",
      (event) => {
        if (event.payload.project_id === project.id) {
          setProgressMsg(event.payload.message);
        }
      }
    );
    return () => { unlisten.then((f) => f()); };
  }, [project.id]);

  // Mark operation completed when status settles
  useEffect(() => {
    if (project.status === "running" || project.status === "stopped" || project.status === "error") {
      if (activeOperation) {
        setOperationCompleted(true);
      }
      // Clear progress if no modal is managing it
      if (!activeOperation) {
        setProgressMsg(null);
      }
    }
  }, [project.status, activeOperation]);

  const handleStart = async () => {
    setLoading(true);
    setError(null);
    setProgressMsg(null);
    setOperationCompleted(false);
    setActiveOperation("starting");
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
    setProgressMsg(null);
    setOperationCompleted(false);
    setActiveOperation("stopping");
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

  const handleOpenBashShell = async () => {
    try {
      await openTerminal(project.id, project.name, "bash");
    } catch (e) {
      setError(String(e));
    }
  };

  const handleForceStop = async () => {
    try {
      await stop(project.id);
    } catch (e) {
      setError(String(e));
    }
  };

  const closeModal = () => {
    setActiveOperation(null);
    setOperationCompleted(false);
    setProgressMsg(null);
    setError(null);
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

  const defaultOllamaConfig: OllamaConfig = {
    base_url: "http://host.docker.internal:11434",
    model_id: null,
  };

  const defaultLiteLlmConfig: LiteLlmConfig = {
    base_url: "http://host.docker.internal:4000",
    api_key: null,
    model_id: null,
  };

  const handleBackendChange = async (mode: Backend) => {
    try {
      const updates: Partial<Project> = { backend: mode };
      if (mode === "bedrock" && !project.bedrock_config) {
        updates.bedrock_config = defaultBedrockConfig;
      }
      if (mode === "ollama" && !project.ollama_config) {
        updates.ollama_config = defaultOllamaConfig;
      }
      if (mode === "lite_llm" && !project.litellm_config) {
        updates.litellm_config = defaultLiteLlmConfig;
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
    } catch (err) {
      console.error("Failed to update Bedrock config:", err);
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

  // Blur handlers for text fields
  const handleSshKeyPathBlur = async () => {
    try {
      await update({ ...project, ssh_key_path: sshKeyPath || null });
    } catch (err) {
      console.error("Failed to update SSH key path:", err);
    }
  };

  const handleGitNameBlur = async () => {
    try {
      await update({ ...project, git_user_name: gitName || null });
    } catch (err) {
      console.error("Failed to update Git name:", err);
    }
  };

  const handleGitEmailBlur = async () => {
    try {
      await update({ ...project, git_user_email: gitEmail || null });
    } catch (err) {
      console.error("Failed to update Git email:", err);
    }
  };

  const handleGitTokenBlur = async () => {
    try {
      await update({ ...project, git_token: gitToken || null });
    } catch (err) {
      console.error("Failed to update Git token:", err);
    }
  };

  const handleBedrockRegionBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, aws_region: bedrockRegion } });
    } catch (err) {
      console.error("Failed to update Bedrock region:", err);
    }
  };

  const handleBedrockAccessKeyIdBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, aws_access_key_id: bedrockAccessKeyId || null } });
    } catch (err) {
      console.error("Failed to update Bedrock access key:", err);
    }
  };

  const handleBedrockSecretKeyBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, aws_secret_access_key: bedrockSecretKey || null } });
    } catch (err) {
      console.error("Failed to update Bedrock secret key:", err);
    }
  };

  const handleBedrockSessionTokenBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, aws_session_token: bedrockSessionToken || null } });
    } catch (err) {
      console.error("Failed to update Bedrock session token:", err);
    }
  };

  const handleBedrockProfileBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, aws_profile: bedrockProfile || null } });
    } catch (err) {
      console.error("Failed to update Bedrock profile:", err);
    }
  };

  const handleBedrockBearerTokenBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, aws_bearer_token: bedrockBearerToken || null } });
    } catch (err) {
      console.error("Failed to update Bedrock bearer token:", err);
    }
  };

  const handleBedrockModelIdBlur = async () => {
    try {
      const current = project.bedrock_config ?? defaultBedrockConfig;
      await update({ ...project, bedrock_config: { ...current, model_id: bedrockModelId || null } });
    } catch (err) {
      console.error("Failed to update Bedrock model ID:", err);
    }
  };

  const handleOllamaBaseUrlBlur = async () => {
    try {
      const current = project.ollama_config ?? defaultOllamaConfig;
      await update({ ...project, ollama_config: { ...current, base_url: ollamaBaseUrl } });
    } catch (err) {
      console.error("Failed to update Ollama base URL:", err);
    }
  };

  const handleOllamaModelIdBlur = async () => {
    try {
      const current = project.ollama_config ?? defaultOllamaConfig;
      await update({ ...project, ollama_config: { ...current, model_id: ollamaModelId || null } });
    } catch (err) {
      console.error("Failed to update Ollama model ID:", err);
    }
  };

  const handleLitellmBaseUrlBlur = async () => {
    try {
      const current = project.litellm_config ?? defaultLiteLlmConfig;
      await update({ ...project, litellm_config: { ...current, base_url: litellmBaseUrl } });
    } catch (err) {
      console.error("Failed to update LiteLLM base URL:", err);
    }
  };

  const handleLitellmApiKeyBlur = async () => {
    try {
      const current = project.litellm_config ?? defaultLiteLlmConfig;
      await update({ ...project, litellm_config: { ...current, api_key: litellmApiKey || null } });
    } catch (err) {
      console.error("Failed to update LiteLLM API key:", err);
    }
  };

  const handleLitellmModelIdBlur = async () => {
    try {
      const current = project.litellm_config ?? defaultLiteLlmConfig;
      await update({ ...project, litellm_config: { ...current, model_id: litellmModelId || null } });
    } catch (err) {
      console.error("Failed to update LiteLLM model ID:", err);
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
      className={`px-3 py-2 rounded cursor-pointer transition-colors min-w-0 overflow-hidden ${
        isSelected
          ? "bg-[var(--bg-tertiary)]"
          : "hover:bg-[var(--bg-tertiary)]"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className={`w-2 h-2 rounded-full flex-shrink-0 ${statusColor}`} />
        {isEditingName ? (
          <input
            autoFocus
            value={editName}
            onChange={(e) => setEditName(e.target.value)}
            onBlur={async () => {
              setIsEditingName(false);
              const trimmed = editName.trim();
              if (trimmed && trimmed !== project.name) {
                try {
                  await update({ ...project, name: trimmed });
                } catch (err) {
                  console.error("Failed to rename project:", err);
                  setEditName(project.name);
                }
              } else {
                setEditName(project.name);
              }
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
              if (e.key === "Escape") { setEditName(project.name); setIsEditingName(false); }
            }}
            onClick={(e) => e.stopPropagation()}
            className="text-sm font-medium flex-1 min-w-0 px-1 py-0 bg-[var(--bg-primary)] border border-[var(--accent)] rounded text-[var(--text-primary)] focus:outline-none"
          />
        ) : (
          <span
            className="text-sm font-medium truncate flex-1 cursor-text"
            title="Double-click to rename"
            onDoubleClick={(e) => { e.stopPropagation(); setIsEditingName(true); }}
          >
            {project.name}
          </span>
        )}
      </div>
      <div className="mt-0.5 ml-4 space-y-0.5">
        {project.paths.map((pp, i) => (
          <div key={i} className="text-xs text-[var(--text-secondary)] truncate">
            <span className="font-mono">/workspace/{pp.mount_name}</span>
          </div>
        ))}
      </div>

      {isSelected && (
        <div className="mt-2 ml-4 space-y-2 min-w-0 overflow-hidden">
          {/* Backend selector */}
          <div className="flex items-center gap-1 text-xs">
            <span className="text-[var(--text-secondary)] mr-1">Backend:<Tooltip text="Anthropic = direct Claude API via OAuth. Bedrock = AWS Bedrock. Ollama = local models. LiteLLM = proxy gateway for 100+ providers." /></span>
            <select
              value={project.backend}
              onChange={(e) => { e.stopPropagation(); handleBackendChange(e.target.value as Backend); }}
              onClick={(e) => e.stopPropagation()}
              disabled={!isStopped}
              className="px-2 py-0.5 rounded bg-[var(--bg-primary)] border border-[var(--border-color)] text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
            >
              <option value="anthropic">Anthropic</option>
              <option value="bedrock">Bedrock</option>
              <option value="ollama">Ollama</option>
              <option value="lite_llm">LiteLLM</option>
            </select>
          </div>

          {/* Action buttons */}
          <div className="flex items-center gap-1 flex-wrap">
            {isStopped ? (
              <>
                <ActionButton onClick={handleStart} disabled={loading} label="Start" />
                <ActionButton
                  onClick={async () => {
                    setLoading(true);
                    setError(null);
                    setProgressMsg(null);
                    setOperationCompleted(false);
                    setActiveOperation("resetting");
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
                <ActionButton onClick={handleOpenBashShell} disabled={loading} label="Shell" />
                <ActionButton onClick={() => setShowFileManager(true)} disabled={loading} label="Files" />
              </>
            ) : (
              <>
                <span className="text-xs text-[var(--text-secondary)]">
                  {progressMsg ?? `${project.status}...`}
                </span>
                <ActionButton onClick={handleStop} disabled={loading} label="Force Stop" danger />
              </>
            )}
            <ActionButton
              onClick={(e) => { e?.stopPropagation?.(); setShowConfig(!showConfig); }}
              disabled={false}
              label={showConfig ? "Hide" : "Config"}
            />
            <ActionButton
              onClick={() => setShowRemoveModal(true)}
              disabled={loading}
              label="Remove"
              danger
            />
          </div>

          {/* Config panel */}
          {showConfig && (
            <div className="space-y-2 pt-1 border-t border-[var(--border-color)] min-w-0 overflow-hidden" onClick={(e) => e.stopPropagation()}>
              {!isStopped && (
                <div className="px-2 py-1.5 bg-[var(--warning)]/15 border border-[var(--warning)]/30 rounded text-xs text-[var(--warning)]">
                  Container must be stopped to change settings.
                </div>
              )}
              {/* Folder paths */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Folders</label>
                {paths.map((pp, i) => (
                  <div key={i} className="mb-1">
                    <div className="flex gap-1 items-center min-w-0">
                      <input
                        value={pp.host_path}
                        onChange={(e) => {
                          const updated = [...paths];
                          updated[i] = { ...updated[i], host_path: e.target.value };
                          setPaths(updated);
                        }}
                        onBlur={async () => {
                          try { await update({ ...project, paths }); } catch (err) {
                            console.error("Failed to update paths:", err);
                          }
                        }}
                        placeholder="/path/to/folder"
                        disabled={!isStopped}
                        className="flex-1 min-w-0 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                      />
                      <button
                        onClick={async () => {
                          const selected = await open({ directory: true, multiple: false });
                          if (typeof selected === "string") {
                            const updated = [...paths];
                            const basename = selected.replace(/[/\\]$/, "").split(/[/\\]/).pop() || "";
                            updated[i] = { host_path: selected, mount_name: updated[i].mount_name || basename };
                            setPaths(updated);
                            try { await update({ ...project, paths: updated }); } catch (err) {
                              console.error("Failed to update paths:", err);
                            }
                          }
                        }}
                        disabled={!isStopped}
                        className="flex-shrink-0 px-2 py-1 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] disabled:opacity-50 transition-colors"
                      >
                        ...
                      </button>
                      {paths.length > 1 && (
                        <button
                          onClick={async () => {
                            const updated = paths.filter((_, j) => j !== i);
                            setPaths(updated);
                            try { await update({ ...project, paths: updated }); } catch (err) {
                              console.error("Failed to remove path:", err);
                            }
                          }}
                          disabled={!isStopped}
                          className="flex-shrink-0 px-1.5 py-1 text-xs text-[var(--error)] hover:bg-[var(--bg-primary)] rounded disabled:opacity-50 transition-colors"
                        >
                          x
                        </button>
                      )}
                    </div>
                    <div className="flex gap-1 items-center mt-0.5 min-w-0">
                      <span className="text-xs text-[var(--text-secondary)]">/workspace/</span>
                      <input
                        value={pp.mount_name}
                        onChange={(e) => {
                          const updated = [...paths];
                          updated[i] = { ...updated[i], mount_name: e.target.value };
                          setPaths(updated);
                        }}
                        onBlur={async () => {
                          try { await update({ ...project, paths }); } catch (err) {
                            console.error("Failed to update paths:", err);
                          }
                        }}
                        placeholder="name"
                        disabled={!isStopped}
                        className="flex-1 min-w-0 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
                      />
                    </div>
                  </div>
                ))}
                <button
                  onClick={async () => {
                    const updated = [...paths, { host_path: "", mount_name: "" }];
                    setPaths(updated);
                  }}
                  disabled={!isStopped}
                  className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] disabled:opacity-50 transition-colors"
                >
                  + Add folder
                </button>
              </div>

              {/* SSH Key */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">SSH Key Directory<Tooltip text="Path to your .ssh directory. Mounted into the container so Claude can authenticate with Git remotes over SSH." /></label>
                <div className="flex gap-1">
                  <input
                    value={sshKeyPath}
                    onChange={(e) => setSshKeyPath(e.target.value)}
                    onBlur={handleSshKeyPathBlur}
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
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Git Name<Tooltip text="Sets git user.name inside the container for commit authorship." /></label>
                <input
                  value={gitName}
                  onChange={(e) => setGitName(e.target.value)}
                  onBlur={handleGitNameBlur}
                  placeholder="Your Name"
                  disabled={!isStopped}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                />
              </div>

              {/* Git Email */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Git Email<Tooltip text="Sets git user.email inside the container for commit authorship." /></label>
                <input
                  value={gitEmail}
                  onChange={(e) => setGitEmail(e.target.value)}
                  onBlur={handleGitEmailBlur}
                  placeholder="you@example.com"
                  disabled={!isStopped}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                />
              </div>

              {/* Git Token (HTTPS) */}
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Git HTTPS Token<Tooltip text="A personal access token (e.g. GitHub PAT) for HTTPS git operations inside the container." /></label>
                <input
                  type="password"
                  value={gitToken}
                  onChange={(e) => setGitToken(e.target.value)}
                  onBlur={handleGitTokenBlur}
                  placeholder="ghp_..."
                  disabled={!isStopped}
                  className="w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                />
              </div>

              {/* Docker access toggle */}
              <div className="flex items-center gap-2">
                <label className="text-xs text-[var(--text-secondary)]">Allow container spawning<Tooltip text="Mounts the Docker socket so Claude can build and run Docker containers from inside the sandbox." /></label>
                <button
                  onClick={async () => {
                    try { await update({ ...project, allow_docker_access: !project.allow_docker_access }); } catch (err) {
                      console.error("Failed to update Docker access setting:", err);
                    }
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

              {/* Mission Control toggle */}
              <div className="flex items-center gap-2">
                <label className="text-xs text-[var(--text-secondary)]">Mission Control<Tooltip text="Enables a web dashboard for monitoring and managing Claude sessions remotely." /></label>
                <button
                  onClick={async () => {
                    try {
                      await update({ ...project, mission_control_enabled: !project.mission_control_enabled });
                    } catch (err) {
                      console.error("Failed to update Mission Control setting:", err);
                    }
                  }}
                  disabled={!isStopped}
                  className={`px-2 py-0.5 text-xs rounded transition-colors disabled:opacity-50 ${
                    project.mission_control_enabled
                      ? "bg-[var(--success)] text-white"
                      : "bg-[var(--bg-primary)] border border-[var(--border-color)] text-[var(--text-secondary)]"
                  }`}
                >
                  {project.mission_control_enabled ? "ON" : "OFF"}
                </button>
              </div>

              {/* Environment Variables */}
              <div className="flex items-center justify-between">
                <label className="text-xs text-[var(--text-secondary)]">
                  Environment Variables{envVars.length > 0 && ` (${envVars.length})`}<Tooltip text="Custom env vars injected into this project's container. Useful for API keys or tool configuration." />
                </label>
                <button
                  onClick={() => setShowEnvVarsModal(true)}
                  className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
                >
                  Edit
                </button>
              </div>

              {/* Port Mappings */}
              <div className="flex items-center justify-between">
                <label className="text-xs text-[var(--text-secondary)]">
                  Port Mappings{portMappings.length > 0 && ` (${portMappings.length})`}<Tooltip text="Map container ports to host ports so you can access dev servers running inside the container." />
                </label>
                <button
                  onClick={() => setShowPortMappingsModal(true)}
                  className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
                >
                  Edit
                </button>
              </div>

              {/* Claude Instructions */}
              <div className="flex items-center justify-between">
                <label className="text-xs text-[var(--text-secondary)]">
                  Claude Instructions{claudeInstructions ? " (set)" : ""}<Tooltip text="Project-specific instructions written to CLAUDE.md. Guides Claude's behavior for this project." />
                </label>
                <button
                  onClick={() => setShowClaudeInstructionsModal(true)}
                  className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
                >
                  Edit
                </button>
              </div>

              {/* MCP Servers */}
              {mcpServers.length > 0 && (
                <div>
                  <label className="block text-xs text-[var(--text-secondary)] mb-1">MCP Servers<Tooltip text="Model Context Protocol servers give Claude access to external tools and data sources." /></label>
                  <div className="space-y-1">
                    {mcpServers.map((server) => {
                      const enabled = project.enabled_mcp_servers.includes(server.id);
                      const isDocker = !!server.docker_image;
                      return (
                        <label key={server.id} className="flex items-center gap-2 cursor-pointer">
                          <input
                            type="checkbox"
                            checked={enabled}
                            disabled={!isStopped}
                            onChange={async () => {
                              const updated = enabled
                                ? project.enabled_mcp_servers.filter((id) => id !== server.id)
                                : [...project.enabled_mcp_servers, server.id];
                              try {
                                await update({ ...project, enabled_mcp_servers: updated });
                              } catch (err) {
                                console.error("Failed to update MCP servers:", err);
                              }
                            }}
                            className="rounded border-[var(--border-color)] disabled:opacity-50"
                          />
                          <span className="text-xs text-[var(--text-primary)]">{server.name}</span>
                          <span className="text-xs text-[var(--text-secondary)]">({server.transport_type})</span>
                          <span className={`text-xs px-1 py-0.5 rounded ${isDocker ? "bg-blue-500/20 text-blue-400" : "bg-[var(--bg-secondary)] text-[var(--text-secondary)]"}`}>
                            {isDocker ? "Docker" : "Manual"}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                  {mcpServers.some((s) => s.docker_image && s.transport_type === "stdio" && project.enabled_mcp_servers.includes(s.id)) && (
                    <p className="text-xs text-[var(--text-secondary)] mt-1 opacity-70">
                      Docker access will be auto-enabled for stdio+Docker MCP servers.
                    </p>
                  )}
                </div>
              )}

              {/* Bedrock config */}
              {project.backend === "bedrock" && (() => {
                const bc = project.bedrock_config ?? defaultBedrockConfig;
                const inputCls = "w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50";
                return (
                  <div className="space-y-2 pt-1 border-t border-[var(--border-color)]">
                    <label className="block text-xs font-medium text-[var(--text-primary)]">AWS Bedrock</label>

                    {/* Sub-method selector */}
                    <div className="flex items-center gap-1 text-xs">
                      <span className="text-[var(--text-secondary)] mr-1">Method:</span>
                      <select
                        value={bc.auth_method}
                        onChange={(e) => updateBedrockConfig({ auth_method: e.target.value as BedrockAuthMethod })}
                        onClick={(e) => e.stopPropagation()}
                        disabled={!isStopped}
                        className="px-2 py-0.5 rounded bg-[var(--bg-primary)] border border-[var(--border-color)] text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
                      >
                        <option value="static_credentials">Keys</option>
                        <option value="profile">Profile</option>
                        <option value="bearer_token">Token</option>
                      </select>
                    </div>

                    {/* AWS Region (always shown) */}
                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">AWS Region<Tooltip text="The AWS region where your Bedrock endpoint is available (e.g. us-east-1)." /></label>
                      <input
                        value={bedrockRegion}
                        onChange={(e) => setBedrockRegion(e.target.value)}
                        onBlur={handleBedrockRegionBlur}
                        placeholder="us-east-1"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                    </div>

                    {/* Static credentials fields */}
                    {bc.auth_method === "static_credentials" && (
                      <>
                        <div>
                          <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Access Key ID<Tooltip text="Your AWS IAM access key ID for Bedrock API authentication." /></label>
                          <input
                            value={bedrockAccessKeyId}
                            onChange={(e) => setBedrockAccessKeyId(e.target.value)}
                            onBlur={handleBedrockAccessKeyIdBlur}
                            placeholder="AKIA..."
                            disabled={!isStopped}
                            className={inputCls}
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Secret Access Key<Tooltip text="Your AWS IAM secret key. Stored locally and injected as an env var into the container." /></label>
                          <input
                            type="password"
                            value={bedrockSecretKey}
                            onChange={(e) => setBedrockSecretKey(e.target.value)}
                            onBlur={handleBedrockSecretKeyBlur}
                            disabled={!isStopped}
                            className={inputCls}
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Session Token (optional)<Tooltip text="Temporary session token for assumed-role or MFA-based AWS credentials." /></label>
                          <input
                            type="password"
                            value={bedrockSessionToken}
                            onChange={(e) => setBedrockSessionToken(e.target.value)}
                            onBlur={handleBedrockSessionTokenBlur}
                            disabled={!isStopped}
                            className={inputCls}
                          />
                        </div>
                      </>
                    )}

                    {/* Profile field */}
                    {bc.auth_method === "profile" && (
                      <div>
                        <label className="block text-xs text-[var(--text-secondary)] mb-0.5">AWS Profile<Tooltip text="Named profile from your AWS config/credentials files (e.g. 'default' or 'prod')." /></label>
                        <input
                          value={bedrockProfile}
                          onChange={(e) => setBedrockProfile(e.target.value)}
                          onBlur={handleBedrockProfileBlur}
                          placeholder="default"
                          disabled={!isStopped}
                          className={inputCls}
                        />
                      </div>
                    )}

                    {/* Bearer token field */}
                    {bc.auth_method === "bearer_token" && (
                      <div>
                        <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Bearer Token<Tooltip text="An SSO or identity-center bearer token for Bedrock authentication." /></label>
                        <input
                          type="password"
                          value={bedrockBearerToken}
                          onChange={(e) => setBedrockBearerToken(e.target.value)}
                          onBlur={handleBedrockBearerTokenBlur}
                          disabled={!isStopped}
                          className={inputCls}
                        />
                      </div>
                    )}

                    {/* Model override */}
                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Model ID (optional)<Tooltip text="Override the default Bedrock model. Leave blank to use Claude's default." /></label>
                      <input
                        value={bedrockModelId}
                        onChange={(e) => setBedrockModelId(e.target.value)}
                        onBlur={handleBedrockModelIdBlur}
                        placeholder="anthropic.claude-sonnet-4-20250514-v1:0"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                    </div>
                  </div>
                );
              })()}

              {/* Ollama config */}
              {project.backend === "ollama" && (() => {
                const inputCls = "w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50";
                return (
                  <div className="space-y-2 pt-1 border-t border-[var(--border-color)]">
                    <label className="block text-xs font-medium text-[var(--text-primary)]">Ollama</label>
                    <p className="text-xs text-[var(--text-secondary)]">
                      Connect to an Ollama server running locally or on a remote host.
                    </p>

                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Base URL<Tooltip text="URL of your Ollama server. Use host.docker.internal to reach the host machine from inside the container." /></label>
                      <input
                        value={ollamaBaseUrl}
                        onChange={(e) => setOllamaBaseUrl(e.target.value)}
                        onBlur={handleOllamaBaseUrlBlur}
                        placeholder="http://host.docker.internal:11434"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                      <p className="text-xs text-[var(--text-secondary)] mt-0.5 opacity-70">
                        Use host.docker.internal for the host machine, or an IP/hostname for remote.
                      </p>
                    </div>

                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Model (optional)<Tooltip text="Ollama model name to use (e.g. qwen3.5:27b). Leave blank for the server default." /></label>
                      <input
                        value={ollamaModelId}
                        onChange={(e) => setOllamaModelId(e.target.value)}
                        onBlur={handleOllamaModelIdBlur}
                        placeholder="qwen3.5:27b"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                    </div>
                  </div>
                );
              })()}

              {/* LiteLLM config */}
              {project.backend === "lite_llm" && (() => {
                const inputCls = "w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50";
                return (
                  <div className="space-y-2 pt-1 border-t border-[var(--border-color)]">
                    <label className="block text-xs font-medium text-[var(--text-primary)]">LiteLLM Gateway</label>
                    <p className="text-xs text-[var(--text-secondary)]">
                      Connect through a LiteLLM proxy to use 100+ model providers.
                    </p>

                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Base URL<Tooltip text="URL of your LiteLLM proxy server. Use host.docker.internal for a locally running proxy." /></label>
                      <input
                        value={litellmBaseUrl}
                        onChange={(e) => setLitellmBaseUrl(e.target.value)}
                        onBlur={handleLitellmBaseUrlBlur}
                        placeholder="http://host.docker.internal:4000"
                        disabled={!isStopped}
                        className={inputCls}
                      />
                      <p className="text-xs text-[var(--text-secondary)] mt-0.5 opacity-70">
                        Use host.docker.internal for local, or a URL for remote/containerized LiteLLM.
                      </p>
                    </div>

                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">API Key<Tooltip text="Authentication key for your LiteLLM proxy, if required." /></label>
                      <input
                        type="password"
                        value={litellmApiKey}
                        onChange={(e) => setLitellmApiKey(e.target.value)}
                        onBlur={handleLitellmApiKeyBlur}
                        placeholder="sk-..."
                        disabled={!isStopped}
                        className={inputCls}
                      />
                    </div>

                    <div>
                      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Model (optional)<Tooltip text="Model identifier as configured in your LiteLLM proxy (e.g. gpt-4o, gemini-pro)." /></label>
                      <input
                        value={litellmModelId}
                        onChange={(e) => setLitellmModelId(e.target.value)}
                        onBlur={handleLitellmModelIdBlur}
                        placeholder="gpt-4o / gemini-pro / etc."
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

      {showEnvVarsModal && (
        <EnvVarsModal
          envVars={envVars}
          disabled={!isStopped}
          onSave={async (vars) => {
            setEnvVars(vars);
            await update({ ...project, custom_env_vars: vars });
          }}
          onClose={() => setShowEnvVarsModal(false)}
        />
      )}

      {showPortMappingsModal && (
        <PortMappingsModal
          portMappings={portMappings}
          disabled={!isStopped}
          onSave={async (mappings) => {
            setPortMappings(mappings);
            await update({ ...project, port_mappings: mappings });
          }}
          onClose={() => setShowPortMappingsModal(false)}
        />
      )}

      {showClaudeInstructionsModal && (
        <ClaudeInstructionsModal
          instructions={claudeInstructions}
          disabled={!isStopped}
          onSave={async (instructions) => {
            setClaudeInstructions(instructions);
            await update({ ...project, claude_instructions: instructions || null });
          }}
          onClose={() => setShowClaudeInstructionsModal(false)}
        />
      )}

      {showFileManager && (
        <FileManagerModal
          projectId={project.id}
          projectName={project.name}
          onClose={() => setShowFileManager(false)}
        />
      )}

      {showRemoveModal && (
        <ConfirmRemoveModal
          projectName={project.name}
          onConfirm={async () => {
            setShowRemoveModal(false);
            await remove(project.id);
          }}
          onCancel={() => setShowRemoveModal(false)}
        />
      )}

      {activeOperation && (
        <ContainerProgressModal
          projectName={project.name}
          operation={activeOperation}
          progressMsg={progressMsg}
          error={error}
          completed={operationCompleted}
          onForceStop={handleForceStop}
          onClose={closeModal}
        />
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

