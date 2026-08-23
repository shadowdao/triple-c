import { useEffect, useState } from "react";
import { useSecretField } from "../../../../hooks/useSecretField";
import { open } from "@tauri-apps/plugin-dialog";
import type { Project } from "../../../../lib/types";
import Button from "../../../ui/Button";
import Field, { ConfigGroup, inputClass } from "../../../ui/Field";
import CaCertPathInput from "../../../settings/CaCertPathInput";
import EnvVarsEditor from "../../EnvVarsEditor";
import PortMappingsEditor from "../../PortMappingsEditor";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
  disabledReason?: string;
}

export default function AccessSection({
  project,
  save,
  disabled,
  disabledReason,
}: Props) {
  const [sshKeyPath, setSshKeyPath] = useState(project.ssh_key_path ?? "");
  const [caCertPath, setCaCertPath] = useState(project.ca_cert_path ?? "");
  const [gitName, setGitName] = useState(project.git_user_name ?? "");
  const [gitEmail, setGitEmail] = useState(project.git_user_email ?? "");
  // Never seeded from `project` — the backend does not serialize secrets, so
  // the box is always empty and only an edit may speak about the stored value.
  const gitToken = useSecretField(project.id);

  useEffect(() => {
    setSshKeyPath(project.ssh_key_path ?? "");
    setCaCertPath(project.ca_cert_path ?? "");
    setGitName(project.git_user_name ?? "");
    setGitEmail(project.git_user_email ?? "");
  }, [project]);

  return (
    <ConfigGroup
      title="Access"
      description="Credentials, environment, and networking the container is given."
    >
      <Field
        label="SSH key directory"
        hint="Mounted into the container so Claude can authenticate with Git remotes over SSH."
      >
        {(id) => (
          <div className="flex gap-1.5">
            <input
              id={id}
              value={sshKeyPath}
              onChange={(e) => setSshKeyPath(e.target.value)}
              onBlur={() => save({ ssh_key_path: sshKeyPath || null })}
              placeholder="~/.ssh"
              disabled={disabled}
              className={inputClass}
            />
            <Button
              size="md"
              disabled={disabled}
              onClick={async () => {
                const selected = await open({ directory: true, multiple: false });
                if (typeof selected === "string") {
                  setSshKeyPath(selected);
                  save({ ssh_key_path: selected });
                }
              }}
            >
              Browse
            </Button>
          </div>
        )}
      </Field>

      <Field label="Git name" hint="Sets git user.name inside the container for commit authorship.">
        {(id) => (
          <input
            id={id}
            value={gitName}
            onChange={(e) => setGitName(e.target.value)}
            onBlur={() => save({ git_user_name: gitName || null })}
            placeholder="Your Name"
            disabled={disabled}
            className={inputClass}
          />
        )}
      </Field>

      <Field label="Git email" hint="Sets git user.email inside the container for commit authorship.">
        {(id) => (
          <input
            id={id}
            value={gitEmail}
            onChange={(e) => setGitEmail(e.target.value)}
            onBlur={() => save({ git_user_email: gitEmail || null })}
            placeholder="you@example.com"
            disabled={disabled}
            className={inputClass}
          />
        )}
      </Field>

      <Field
        label="Git HTTPS token"
        hint={
          gitToken.edited
            ? "Saved when you click away. Clearing the box removes the stored token."
            : "A personal access token (e.g. a GitHub PAT) for HTTPS git operations inside the container. A stored token is not shown; leave this empty to keep it."
        }
      >
        {(id) => (
          <input
            id={id}
            type="password"
            value={gitToken.value}
            onChange={(e) => gitToken.setValue(e.target.value)}
            onBlur={() => save({ ...gitToken.patch("git_token") })}
            placeholder="ghp_…"
            disabled={disabled}
            className={inputClass}
          />
        )}
      </Field>

      <Field
        label="Corporate CA certificate"
        hint="Overrides the global certificate for this project only. A certificate file, or a folder of them, trusted inside the container by curl, git, npm, pip, Chromium and Claude Code."
      >
        {(id) => (
          <CaCertPathInput
            id={id}
            value={caCertPath}
            onChange={setCaCertPath}
            onCommit={(value) => save({ ca_cert_path: value.trim() || null })}
            disabled={disabled}
            placeholder="/etc/ssl/certs/corp-root.pem"
            emptyHint="Using the global certificate from Settings → Certificates."
            inputClassName={`${inputClass} min-w-0`}
          />
        )}
      </Field>

      <div className="pt-2 border-t border-[var(--border-color)]">
        <span className="block text-[13px] font-medium text-[var(--text-primary)]">
          Environment variables
        </span>
        <p className="mt-0.5 mb-2 text-xs text-[var(--text-secondary)] leading-snug">
          Injected into this project&rsquo;s container. These override global variables
          with the same key.
        </p>
        <EnvVarsEditor
          envVars={project.custom_env_vars ?? []}
          disabled={disabled}
          disabledReason={disabledReason}
          onSave={(vars) => save({ custom_env_vars: vars })}
        />
      </div>

      <div className="pt-2 border-t border-[var(--border-color)]">
        <span className="block text-[13px] font-medium text-[var(--text-primary)]">
          Port mappings
        </span>
        <p className="mt-0.5 mb-2 text-xs text-[var(--text-secondary)] leading-snug">
          Expose container ports on the host so you can reach dev servers running inside
          the sandbox.
        </p>
        <PortMappingsEditor
          portMappings={project.port_mappings ?? []}
          disabled={disabled}
          disabledReason={disabledReason}
          onSave={(mappings) => save({ port_mappings: mappings })}
        />
      </div>
    </ConfigGroup>
  );
}
