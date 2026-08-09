import { useEffect, useState } from "react";
import type {
  Backend,
  BedrockAuthMethod,
  BedrockConfig,
  OllamaConfig,
  OpenAiCompatibleConfig,
  Project,
} from "../../../../lib/types";
import Field, {
  ConfigGroup,
  SwitchRow,
  monoInputClass,
  selectClass,
} from "../../../ui/Field";
import Toggle from "../../../ui/Toggle";

export const DEFAULT_BEDROCK_CONFIG: BedrockConfig = {
  auth_method: "static_credentials",
  aws_region: "us-east-1",
  aws_access_key_id: null,
  aws_secret_access_key: null,
  aws_session_token: null,
  aws_profile: null,
  aws_bearer_token: null,
  model_id: null,
  disable_prompt_caching: false,
  service_tier: null,
};

export const DEFAULT_OLLAMA_CONFIG: OllamaConfig = {
  base_url: "http://host.docker.internal:11434",
  model_id: null,
};

export const DEFAULT_OPENAI_COMPATIBLE_CONFIG: OpenAiCompatibleConfig = {
  base_url: "http://host.docker.internal:4000",
  api_key: null,
  model_id: null,
};

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
}

export default function ModelSection({ project, save, disabled }: Props) {
  const bedrock = project.bedrock_config ?? DEFAULT_BEDROCK_CONFIG;

  // Local text state — saved on blur, not on every keystroke.
  const [bedrockRegion, setBedrockRegion] = useState(bedrock.aws_region);
  const [accessKeyId, setAccessKeyId] = useState(bedrock.aws_access_key_id ?? "");
  const [secretKey, setSecretKey] = useState(bedrock.aws_secret_access_key ?? "");
  const [sessionToken, setSessionToken] = useState(bedrock.aws_session_token ?? "");
  const [profile, setProfile] = useState(bedrock.aws_profile ?? "");
  const [bearerToken, setBearerToken] = useState(bedrock.aws_bearer_token ?? "");
  const [bedrockModelId, setBedrockModelId] = useState(bedrock.model_id ?? "");
  const [serviceTier, setServiceTier] = useState(bedrock.service_tier ?? "");

  const [ollamaBaseUrl, setOllamaBaseUrl] = useState(
    project.ollama_config?.base_url ?? DEFAULT_OLLAMA_CONFIG.base_url,
  );
  const [ollamaModelId, setOllamaModelId] = useState(
    project.ollama_config?.model_id ?? "",
  );

  const [oaiBaseUrl, setOaiBaseUrl] = useState(
    project.openai_compatible_config?.base_url ??
      DEFAULT_OPENAI_COMPATIBLE_CONFIG.base_url,
  );
  const [oaiApiKey, setOaiApiKey] = useState(
    project.openai_compatible_config?.api_key ?? "",
  );
  const [oaiModelId, setOaiModelId] = useState(
    project.openai_compatible_config?.model_id ?? "",
  );

  useEffect(() => {
    const bc = project.bedrock_config ?? DEFAULT_BEDROCK_CONFIG;
    setBedrockRegion(bc.aws_region);
    setAccessKeyId(bc.aws_access_key_id ?? "");
    setSecretKey(bc.aws_secret_access_key ?? "");
    setSessionToken(bc.aws_session_token ?? "");
    setProfile(bc.aws_profile ?? "");
    setBearerToken(bc.aws_bearer_token ?? "");
    setBedrockModelId(bc.model_id ?? "");
    setServiceTier(bc.service_tier ?? "");
    setOllamaBaseUrl(project.ollama_config?.base_url ?? DEFAULT_OLLAMA_CONFIG.base_url);
    setOllamaModelId(project.ollama_config?.model_id ?? "");
    setOaiBaseUrl(
      project.openai_compatible_config?.base_url ??
        DEFAULT_OPENAI_COMPATIBLE_CONFIG.base_url,
    );
    setOaiApiKey(project.openai_compatible_config?.api_key ?? "");
    setOaiModelId(project.openai_compatible_config?.model_id ?? "");
  }, [project]);

  const saveBedrock = (patch: Partial<BedrockConfig>) =>
    save({ bedrock_config: { ...bedrock, ...patch } });

  const saveOllama = (patch: Partial<OllamaConfig>) =>
    save({
      ollama_config: { ...(project.ollama_config ?? DEFAULT_OLLAMA_CONFIG), ...patch },
    });

  const saveOpenAi = (patch: Partial<OpenAiCompatibleConfig>) =>
    save({
      openai_compatible_config: {
        ...(project.openai_compatible_config ?? DEFAULT_OPENAI_COMPATIBLE_CONFIG),
        ...patch,
      },
    });

  // Defaults to on: projects created before the field existed, and any data
  // that predates it, should still pick the shared token up.
  const useSharedToken = project.use_shared_auth_token !== false;

  const handleBackendChange = (mode: Backend) => {
    const patch: Partial<Project> = { backend: mode };
    if (mode === "bedrock" && !project.bedrock_config)
      patch.bedrock_config = DEFAULT_BEDROCK_CONFIG;
    if (mode === "ollama" && !project.ollama_config)
      patch.ollama_config = DEFAULT_OLLAMA_CONFIG;
    if (mode === "open_ai_compatible" && !project.openai_compatible_config)
      patch.openai_compatible_config = DEFAULT_OPENAI_COMPATIBLE_CONFIG;
    save(patch);
  };

  return (
    <ConfigGroup title="Model" description="Which provider serves this project's Claude.">
      <Field
        label="Backend"
        hint="Anthropic connects directly via OAuth (run `claude login` in a terminal). Bedrock routes through AWS. Ollama and OpenAI Compatible point at any compatible endpoint."
      >
        {(id) => (
          <select
            id={id}
            value={project.backend}
            onChange={(e) => handleBackendChange(e.target.value as Backend)}
            disabled={disabled}
            className={selectClass}
          >
            <option value="anthropic">Anthropic</option>
            <option value="bedrock">Bedrock</option>
            <option value="ollama">Ollama</option>
            <option value="open_ai_compatible">OpenAI Compatible</option>
          </select>
        )}
      </Field>

      {/* Only Anthropic reads CLAUDE_CODE_OAUTH_TOKEN; the other backends
          authenticate through their own credentials entirely. */}
      {project.backend === "anthropic" && (
        <div className="pt-2 border-t border-[var(--border-color)]">
          <SwitchRow
            label="Use the shared Claude token"
            hint={
              useSharedToken
                ? "Signs in with the shared token from Settings → Claude Authentication, so this container needs no `claude login` of its own."
                : "This project is opted out: it ignores the shared token and needs its own `claude login` inside the container."
            }
            control={
              <Toggle
                label="Use the shared Claude token"
                checked={useSharedToken}
                onChange={(value) => save({ use_shared_auth_token: value })}
                disabled={disabled}
              />
            }
          />
        </div>
      )}

      {project.backend === "bedrock" && (
        <div className="space-y-4 pt-2 border-t border-[var(--border-color)]">
          <Field label="Authentication method" hint="How the container proves its identity to Bedrock.">
            {(id) => (
              <select
                id={id}
                value={bedrock.auth_method}
                onChange={(e) =>
                  saveBedrock({ auth_method: e.target.value as BedrockAuthMethod })
                }
                disabled={disabled}
                className={selectClass}
              >
                <option value="static_credentials">Static keys</option>
                <option value="profile">Named profile</option>
                <option value="bearer_token">Bearer token</option>
              </select>
            )}
          </Field>

          <Field label="AWS region" hint="Region where your Bedrock endpoint is available.">
            {(id) => (
              <input
                id={id}
                value={bedrockRegion}
                onChange={(e) => setBedrockRegion(e.target.value)}
                onBlur={() => saveBedrock({ aws_region: bedrockRegion })}
                placeholder="us-east-1"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>

          {bedrock.auth_method === "static_credentials" && (
            <>
              <Field label="Access key ID" hint="IAM access key used for Bedrock API calls.">
                {(id) => (
                  <input
                    id={id}
                    value={accessKeyId}
                    onChange={(e) => setAccessKeyId(e.target.value)}
                    onBlur={() => saveBedrock({ aws_access_key_id: accessKeyId || null })}
                    placeholder="AKIA…"
                    disabled={disabled}
                    className={monoInputClass}
                  />
                )}
              </Field>
              <Field
                label="Secret access key"
                hint="Stored locally and injected as an env var into the container."
              >
                {(id) => (
                  <input
                    id={id}
                    type="password"
                    value={secretKey}
                    onChange={(e) => setSecretKey(e.target.value)}
                    onBlur={() =>
                      saveBedrock({ aws_secret_access_key: secretKey || null })
                    }
                    disabled={disabled}
                    className={monoInputClass}
                  />
                )}
              </Field>
              <Field
                label="Session token"
                hint="Optional — for assumed-role or MFA-based credentials."
              >
                {(id) => (
                  <input
                    id={id}
                    type="password"
                    value={sessionToken}
                    onChange={(e) => setSessionToken(e.target.value)}
                    onBlur={() =>
                      saveBedrock({ aws_session_token: sessionToken || null })
                    }
                    disabled={disabled}
                    className={monoInputClass}
                  />
                )}
              </Field>
            </>
          )}

          {bedrock.auth_method === "profile" && (
            <Field
              label="AWS profile"
              hint="Named profile from your AWS config/credentials files."
            >
              {(id) => (
                <input
                  id={id}
                  value={profile}
                  onChange={(e) => setProfile(e.target.value)}
                  onBlur={() => saveBedrock({ aws_profile: profile || null })}
                  placeholder="default"
                  disabled={disabled}
                  className={monoInputClass}
                />
              )}
            </Field>
          )}

          {bedrock.auth_method === "bearer_token" && (
            <Field
              label="Bearer token"
              hint="SSO or identity-center token for Bedrock authentication."
            >
              {(id) => (
                <input
                  id={id}
                  type="password"
                  value={bearerToken}
                  onChange={(e) => setBearerToken(e.target.value)}
                  onBlur={() => saveBedrock({ aws_bearer_token: bearerToken || null })}
                  disabled={disabled}
                  className={monoInputClass}
                />
              )}
            </Field>
          )}

          <Field label="Model ID" hint="Optional override. Leave blank for Claude's default.">
            {(id) => (
              <input
                id={id}
                value={bedrockModelId}
                onChange={(e) => setBedrockModelId(e.target.value)}
                onBlur={() => saveBedrock({ model_id: bedrockModelId || null })}
                placeholder="anthropic.claude-sonnet-4-20250514-v1:0"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>

          <Field
            label="Service tier"
            hint="Optional — sets ANTHROPIC_BEDROCK_SERVICE_TIER (e.g. “priority”)."
          >
            {(id) => (
              <input
                id={id}
                value={serviceTier}
                onChange={(e) => setServiceTier(e.target.value)}
                onBlur={() => saveBedrock({ service_tier: serviceTier.trim() || null })}
                placeholder="(account default)"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
        </div>
      )}

      {project.backend === "ollama" && (
        <div className="space-y-4 pt-2 border-t border-[var(--border-color)]">
          <Field
            label="Base URL"
            hint="Use host.docker.internal to reach the host machine, or an IP/hostname for a remote server."
          >
            {(id) => (
              <input
                id={id}
                value={ollamaBaseUrl}
                onChange={(e) => setOllamaBaseUrl(e.target.value)}
                onBlur={() => saveOllama({ base_url: ollamaBaseUrl })}
                placeholder="http://host.docker.internal:11434"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
          <Field
            label="Model"
            hint="Required. The model must already be pulled in Ollama before the container starts."
          >
            {(id) => (
              <input
                id={id}
                value={ollamaModelId}
                onChange={(e) => setOllamaModelId(e.target.value)}
                onBlur={() => saveOllama({ model_id: ollamaModelId || null })}
                placeholder="qwen3.5:27b"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
        </div>
      )}

      {project.backend === "open_ai_compatible" && (
        <div className="space-y-4 pt-2 border-t border-[var(--border-color)]">
          <Field
            label="Base URL"
            hint="Any OpenAI API-compatible endpoint — LiteLLM, OpenRouter, vLLM, and so on."
          >
            {(id) => (
              <input
                id={id}
                value={oaiBaseUrl}
                onChange={(e) => setOaiBaseUrl(e.target.value)}
                onBlur={() => saveOpenAi({ base_url: oaiBaseUrl })}
                placeholder="http://host.docker.internal:4000"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
          <Field label="API key" hint="Authentication key for the endpoint, if it requires one.">
            {(id) => (
              <input
                id={id}
                type="password"
                value={oaiApiKey}
                onChange={(e) => setOaiApiKey(e.target.value)}
                onBlur={() => saveOpenAi({ api_key: oaiApiKey || null })}
                placeholder="sk-…"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
          <Field label="Model" hint="Optional — model identifier as configured by your provider.">
            {(id) => (
              <input
                id={id}
                value={oaiModelId}
                onChange={(e) => setOaiModelId(e.target.value)}
                onBlur={() => saveOpenAi({ model_id: oaiModelId || null })}
                placeholder="gpt-4o / gemini-pro / …"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
        </div>
      )}
    </ConfigGroup>
  );
}
