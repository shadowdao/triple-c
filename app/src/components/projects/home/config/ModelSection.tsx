import { useEffect, useState } from "react";
import { useSecretField, withoutUntouchedSecrets } from "../../../../hooks/useSecretField";
import type {
  Backend,
  BedrockAuthMethod,
  BedrockConfig,
  LlamaCppConfig,
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

/** Bedrock fields held in the OS keychain, never serialized back to us. */
const BEDROCK_SECRET_KEYS = [
  "aws_access_key_id",
  "aws_secret_access_key",
  "aws_session_token",
  "aws_bearer_token",
] as const;

/** The same, for the OpenAI-compatible backend. */
const OPENAI_SECRET_KEYS = ["api_key"] as const;

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
  haiku_model_id: null,
};

/** `llama-server` listens on port 8080 unless `--port` says otherwise. */
export const DEFAULT_LLAMACPP_CONFIG: LlamaCppConfig = {
  base_url: "http://host.docker.internal:8080",
  model_id: null,
  haiku_model_id: null,
};

export const DEFAULT_OPENAI_COMPATIBLE_CONFIG: OpenAiCompatibleConfig = {
  base_url: "http://host.docker.internal:4000",
  api_key: null,
  model_id: null,
  haiku_model_id: null,
};

/** Shown under the optional per-backend Haiku override. Kept in one place so
 *  all three custom-endpoint backends explain it identically. */
const HAIKU_HINT =
  "Optional. Claude Code resolves the `haiku` alias to this, and uses it for background work such as conversation titles. Leave blank to reuse the model above — that is what stops background calls failing against a server that only serves one model.";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
}

export default function ModelSection({ project, save, disabled }: Props) {
  const bedrock = project.bedrock_config ?? DEFAULT_BEDROCK_CONFIG;

  // Local text state — saved on blur, not on every keystroke.
  const [bedrockRegion, setBedrockRegion] = useState(bedrock.aws_region);
  // Secrets are never seeded from `project` — see `useSecretField`.
  const accessKeyId = useSecretField(project.id);
  const secretKey = useSecretField(project.id);
  const sessionToken = useSecretField(project.id);
  const [profile, setProfile] = useState(bedrock.aws_profile ?? "");
  const bearerToken = useSecretField(project.id);
  const [bedrockModelId, setBedrockModelId] = useState(bedrock.model_id ?? "");
  const [serviceTier, setServiceTier] = useState(bedrock.service_tier ?? "");

  const [ollamaBaseUrl, setOllamaBaseUrl] = useState(
    project.ollama_config?.base_url ?? DEFAULT_OLLAMA_CONFIG.base_url,
  );
  const [ollamaModelId, setOllamaModelId] = useState(
    project.ollama_config?.model_id ?? "",
  );
  const [ollamaHaikuModelId, setOllamaHaikuModelId] = useState(
    project.ollama_config?.haiku_model_id ?? "",
  );

  const [llamaCppBaseUrl, setLlamaCppBaseUrl] = useState(
    project.llamacpp_config?.base_url ?? DEFAULT_LLAMACPP_CONFIG.base_url,
  );
  const [llamaCppModelId, setLlamaCppModelId] = useState(
    project.llamacpp_config?.model_id ?? "",
  );
  const [llamaCppHaikuModelId, setLlamaCppHaikuModelId] = useState(
    project.llamacpp_config?.haiku_model_id ?? "",
  );

  const [oaiBaseUrl, setOaiBaseUrl] = useState(
    project.openai_compatible_config?.base_url ??
      DEFAULT_OPENAI_COMPATIBLE_CONFIG.base_url,
  );
  const oaiApiKey = useSecretField(project.id);
  const [oaiModelId, setOaiModelId] = useState(
    project.openai_compatible_config?.model_id ?? "",
  );
  const [oaiHaikuModelId, setOaiHaikuModelId] = useState(
    project.openai_compatible_config?.haiku_model_id ?? "",
  );

  // Secret fields are deliberately absent here: `useSecretField` owns its own
  // reset, and re-seeding one from `project` would write an empty string over
  // whatever the user had half-typed on any unrelated project update.
  useEffect(() => {
    const bc = project.bedrock_config ?? DEFAULT_BEDROCK_CONFIG;
    setBedrockRegion(bc.aws_region);
    setProfile(bc.aws_profile ?? "");
    setBedrockModelId(bc.model_id ?? "");
    setServiceTier(bc.service_tier ?? "");
    setOllamaBaseUrl(project.ollama_config?.base_url ?? DEFAULT_OLLAMA_CONFIG.base_url);
    setOllamaModelId(project.ollama_config?.model_id ?? "");
    setOllamaHaikuModelId(project.ollama_config?.haiku_model_id ?? "");
    setLlamaCppBaseUrl(
      project.llamacpp_config?.base_url ?? DEFAULT_LLAMACPP_CONFIG.base_url,
    );
    setLlamaCppModelId(project.llamacpp_config?.model_id ?? "");
    setLlamaCppHaikuModelId(project.llamacpp_config?.haiku_model_id ?? "");
    setOaiBaseUrl(
      project.openai_compatible_config?.base_url ??
        DEFAULT_OPENAI_COMPATIBLE_CONFIG.base_url,
    );
    setOaiModelId(project.openai_compatible_config?.model_id ?? "");
    setOaiHaikuModelId(project.openai_compatible_config?.haiku_model_id ?? "");
  }, [project]);

  const saveBedrock = (patch: Partial<BedrockConfig>) =>
    save({
      bedrock_config: withoutUntouchedSecrets(
        { ...bedrock, ...patch },
        patch,
        BEDROCK_SECRET_KEYS,
      ),
    });

  const saveOllama = (patch: Partial<OllamaConfig>) =>
    save({
      ollama_config: { ...(project.ollama_config ?? DEFAULT_OLLAMA_CONFIG), ...patch },
    });

  const saveLlamaCpp = (patch: Partial<LlamaCppConfig>) =>
    save({
      llamacpp_config: {
        ...(project.llamacpp_config ?? DEFAULT_LLAMACPP_CONFIG),
        ...patch,
      },
    });

  const saveOpenAi = (patch: Partial<OpenAiCompatibleConfig>) =>
    save({
      openai_compatible_config: withoutUntouchedSecrets(
        {
          ...(project.openai_compatible_config ?? DEFAULT_OPENAI_COMPATIBLE_CONFIG),
          ...patch,
        },
        patch,
        OPENAI_SECRET_KEYS,
      ),
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
    if (mode === "llama_cpp" && !project.llamacpp_config)
      patch.llamacpp_config = DEFAULT_LLAMACPP_CONFIG;
    if (mode === "open_ai_compatible" && !project.openai_compatible_config)
      patch.openai_compatible_config = DEFAULT_OPENAI_COMPATIBLE_CONFIG;
    save(patch);
  };

  return (
    <ConfigGroup title="Model" description="Which provider serves this project's Claude.">
      <Field
        label="Backend"
        hint="Anthropic connects directly via OAuth (run `claude login` in a terminal). Bedrock routes through AWS. Ollama, llama.cpp and OpenAI Compatible point at any endpoint that implements the Anthropic Messages API."
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
            <option value="llama_cpp">llama.cpp</option>
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
                    value={accessKeyId.value}
                    onChange={(e) => accessKeyId.setValue(e.target.value)}
                    onBlur={() => saveBedrock(accessKeyId.patch("aws_access_key_id"))}
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
                    value={secretKey.value}
                    onChange={(e) => secretKey.setValue(e.target.value)}
                    onBlur={() =>
                      saveBedrock(secretKey.patch("aws_secret_access_key"))
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
                    value={sessionToken.value}
                    onChange={(e) => sessionToken.setValue(e.target.value)}
                    onBlur={() =>
                      saveBedrock(sessionToken.patch("aws_session_token"))
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
                  value={bearerToken.value}
                  onChange={(e) => bearerToken.setValue(e.target.value)}
                  onBlur={() => saveBedrock(bearerToken.patch("aws_bearer_token"))}
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
          <Field label="Background model" hint={HAIKU_HINT}>
            {(id) => (
              <input
                id={id}
                value={ollamaHaikuModelId}
                onChange={(e) => setOllamaHaikuModelId(e.target.value)}
                onBlur={() =>
                  saveOllama({ haiku_model_id: ollamaHaikuModelId.trim() || null })
                }
                placeholder="(same as the model above)"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
        </div>
      )}

      {project.backend === "llama_cpp" && (
        <div className="space-y-4 pt-2 border-t border-[var(--border-color)]">
          <Field
            label="Base URL"
            hint="Your llama-server. It listens on port 8080 by default; use host.docker.internal to reach the host machine."
          >
            {(id) => (
              <input
                id={id}
                value={llamaCppBaseUrl}
                onChange={(e) => setLlamaCppBaseUrl(e.target.value)}
                onBlur={() => saveLlamaCpp({ base_url: llamaCppBaseUrl })}
                placeholder="http://host.docker.internal:8080"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
          <Field
            label="Model"
            hint="The model llama-server was started with. llama-server serves one model, so this is mainly what Claude Code reports — but it is also what the model aliases are pinned to."
          >
            {(id) => (
              <input
                id={id}
                value={llamaCppModelId}
                onChange={(e) => setLlamaCppModelId(e.target.value)}
                onBlur={() => saveLlamaCpp({ model_id: llamaCppModelId || null })}
                placeholder="qwen3.5-coder-30b"
                disabled={disabled}
                className={monoInputClass}
              />
            )}
          </Field>
          <Field label="Background model" hint={HAIKU_HINT}>
            {(id) => (
              <input
                id={id}
                value={llamaCppHaikuModelId}
                onChange={(e) => setLlamaCppHaikuModelId(e.target.value)}
                onBlur={() =>
                  saveLlamaCpp({ haiku_model_id: llamaCppHaikuModelId.trim() || null })
                }
                placeholder="(same as the model above)"
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
            hint="A gateway that implements the Anthropic Messages API (POST /v1/messages) — LiteLLM, for example. An endpoint that only speaks OpenAI /v1/chat/completions will not work."
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
                value={oaiApiKey.value}
                onChange={(e) => oaiApiKey.setValue(e.target.value)}
                onBlur={() => saveOpenAi(oaiApiKey.patch("api_key"))}
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
          <Field label="Background model" hint={HAIKU_HINT}>
            {(id) => (
              <input
                id={id}
                value={oaiHaikuModelId}
                onChange={(e) => setOaiHaikuModelId(e.target.value)}
                onBlur={() =>
                  saveOpenAi({ haiku_model_id: oaiHaikuModelId.trim() || null })
                }
                placeholder="(same as the model above)"
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
