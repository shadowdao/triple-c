import { useEffect, useRef, useState } from "react";
import { useSettings } from "../../hooks/useSettings";
import {
  addAwsKeysConnection,
  addAwsProfileConnection,
  awsConnectionIdentity,
  awsConnectionSsoLogin,
  awsPolicyDocument,
  checkAwsPermissions,
  listAwsProfiles,
  removeHostConnection,
  updateAwsConnection,
} from "../../lib/tauri-commands";
import { toAwsError } from "../../lib/aws-error";
import type {
  AwsError,
  HostConnection,
  PermissionReport,
  PermissionSummary,
  PolicyFile,
  Tier,
} from "../../lib/types";
import Button from "../ui/Button";
import SegmentedControl from "../ui/SegmentedControl";
import Field, { inputClass, selectClass } from "../ui/Field";

const COMMON_REGIONS = [
  "us-east-1", "us-east-2", "us-west-1", "us-west-2", "ca-central-1",
  "eu-west-1", "eu-west-2", "eu-west-3", "eu-central-1", "eu-north-1",
  "ap-southeast-1", "ap-southeast-2", "ap-northeast-1", "ap-northeast-2", "ap-south-1",
  "sa-east-1",
];

const OWN_PRINCIPAL_ACTIONS = ["iam:GetRole", "iam:SimulatePrincipalPolicy"];

/** Human-readable copy for a failed AWS command; never raw JSON. */
export function awsErrorMessage(e: AwsError): string {
  switch (e.kind) {
    case "sso_expired":
      return `Your SSO session for profile ${e.profile} has expired.`;
    case "profile_not_found":
      return `Profile ${e.profile} was not found in your AWS config, or has no credentials.`;
    case "keys_missing":
      return "The access keys for this connection are missing from the keychain. Remove and re-add the connection.";
    default:
      return e.message;
  }
}

const SUMMARY_LABEL: Record<PermissionSummary, string> = {
  environment_admin: "Environment admin",
  workspace_user: "Workspace user",
  insufficient: "Insufficient",
};

const SUMMARY_STYLE: Record<PermissionSummary, string> = {
  environment_admin: "text-[var(--success)] border-[var(--success)]/40",
  workspace_user: "text-[var(--accent)] border-[var(--accent)]/40",
  insufficient: "text-[var(--error)] border-[var(--error)]/40",
};

type AuthChoice = "profile" | "keys";

/** An error with the step that failed, so Sign in can retry just that step. */
interface Failure {
  error: AwsError;
  retry: () => Promise<void>;
}

function ErrorLine({
  failure,
  onSignIn,
  signingIn,
}: {
  failure: Failure;
  /** Absent when there is no saved connection to sign in with. */
  onSignIn?: () => void;
  signingIn: boolean;
}) {
  return (
    <div role="alert" className="text-xs text-[var(--error)] leading-snug flex items-center gap-2 flex-wrap">
      <span>{awsErrorMessage(failure.error)}</span>
      {failure.error.kind === "sso_expired" && !onSignIn && (
        <span>
          Sign in with <code>aws sso login --profile {failure.error.profile}</code> and try again.
        </span>
      )}
      {failure.error.kind === "sso_expired" && onSignIn && (
        <Button size="sm" variant="secondary" onClick={onSignIn} disabled={signingIn}>
          {signingIn ? "Signing in…" : "Sign in"}
        </Button>
      )}
    </div>
  );
}

/** A tier's policy files, loaded on demand: each one shown and copied on its own. */
function PolicyFiles({
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

function PermissionReportView({ report }: { report: PermissionReport }) {
  const [copyNote, setCopyNote] = useState<string | null>(null);
  const probed = typeof report.method === "object";
  // Insufficient only because checks couldn't complete (no denied row in the
  // workspace tier, only rows with a note): don't claim the permissions are missing.
  const workspaceTier = report.tiers.find((t) => t.tier === "workspace_user");
  const incomplete =
    report.summary === "insufficient" &&
    !!workspaceTier &&
    !workspaceTier.results.some((r) => r.verdict === "denied") &&
    workspaceTier.results.some((r) => r.note);
  const badge = incomplete
    ? "Incomplete — check again"
    : report.summary === "workspace_user" && probed
      ? "Workspace user (unverified)"
      : SUMMARY_LABEL[report.summary];
  const badgeStyle = incomplete
    ? "text-[var(--text-secondary)] border-[var(--border-color)]"
    : SUMMARY_STYLE[report.summary];
  const flagged = report.tiers
    .map((t) => ({ ...t, results: t.results.filter((r) => r.verdict !== "allowed") }))
    .filter((t) => t.results.length > 0);
  const ownPrincipalDenied =
    report.summary === "insufficient" &&
    report.tiers.some((t) =>
      t.results.some((r) => r.verdict === "denied" && OWN_PRINCIPAL_ACTIONS.includes(r.action)),
    );

  const copy = async (file: PolicyFile) => {
    try {
      await navigator.clipboard.writeText(file.json);
      setCopyNote(`${file.name} copied to clipboard.`);
    } catch (e) {
      setCopyNote(`Could not copy ${file.name}: ${awsErrorMessage(toAwsError(e))}`);
    }
  };

  return (
    <div className="space-y-2 pt-2" data-testid="permission-report">
      <div className="flex items-center gap-2 flex-wrap">
        <span
          className={`px-2 py-0.5 text-xs font-medium rounded-full border ${badgeStyle}`}
        >
          {badge}
        </span>
        <span className="text-xs text-[var(--text-secondary)]">
          {report.identity.arn} · {report.region}
        </span>
      </div>
      {incomplete && (
        <p className="text-xs text-[var(--text-secondary)] leading-snug">
          Some checks couldn't complete (for example because AWS throttled the requests), so the
          permissions couldn't be fully assessed. Check again in a moment.
        </p>
      )}
      {typeof report.method === "object" && (
        <p className="text-xs text-[var(--text-secondary)] leading-snug">
          {`Simulation not permitted (${report.method.probed.reason}). Read access was probed; write actions are unverified and will be confirmed on first use.`}
        </p>
      )}
      {ownPrincipalDenied && (
        <p className="text-xs text-[var(--warning)] leading-snug">
          Triple-C couldn't inspect the caller's own permissions. Both policies grant iam:GetRole and
          iam:SimulatePrincipalPolicy on the caller's own principal, so this is the first thing to ask
          an admin for.
        </p>
      )}
      {flagged.map((t) => (
        <details key={t.tier} className="text-xs">
          <summary className="cursor-pointer text-[var(--text-primary)]">
            {t.tier === "workspace_user" ? "Workspace user" : "Environment admin"} ({t.results.length} not
            allowed)
          </summary>
          <ul className="mt-1 space-y-0.5 pl-4">
            {t.results.map((r) => (
              <li key={r.action} className="text-[var(--text-secondary)]">
                <span className="font-mono">{r.action}</span> — {r.verdict}
                {r.note && (
                  <>
                    {" "}
                    ({r.note}). Check again in a moment.
                  </>
                )}
              </li>
            ))}
          </ul>
        </details>
      ))}
      <PolicyFiles tier="workspace_user" label="Show workspace-user policy" onCopy={copy} />
      <PolicyFiles tier="environment_admin" label="Show admin policies" onCopy={copy} />
      {copyNote && (
        <p role="status" className="text-xs text-[var(--text-secondary)]">
          {copyNote}
        </p>
      )}
      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        The simulation doesn't account for SCPs or session policies.
      </p>
    </div>
  );
}

function ConnectionRow({ host }: { host: HostConnection }) {
  const { loadSettings, setAppSettings } = useSettings();
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(host.name);
  const [region, setRegion] = useState(host.kind.default_region);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [busy, setBusy] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const [report, setReport] = useState<PermissionReport | null>(null);
  const [failure, setFailure] = useState<Failure | null>(null);
  const auth = host.kind.auth;
  const rowRef = useRef<HTMLLIElement>(null);
  const focusIn = (key: string) =>
    rowRef.current?.querySelector<HTMLElement>(`[data-focus="${key}"]`)?.focus();
  useEffect(() => {
    if (confirmRemove) focusIn("confirm-remove");
  }, [confirmRemove]);
  const cancelRemove = () => {
    setConfirmRemove(false);
    requestAnimationFrame(() => focusIn("remove"));
  };

  const runCheck = async () => {
    setBusy(true);
    setFailure(null);
    try {
      setReport(await checkAwsPermissions(host.id));
    } catch (e) {
      setReport(null);
      setFailure({ error: toAwsError(e), retry: runCheck });
    } finally {
      setBusy(false);
    }
  };

  const signIn = async () => {
    if (!failure) return;
    const retry = failure.retry;
    setSigningIn(true);
    try {
      await awsConnectionSsoLogin(host.id);
      setFailure(null);
      await retry();
    } catch (e) {
      setFailure({ error: toAwsError(e), retry });
    } finally {
      setSigningIn(false);
    }
  };

  const save = async () => {
    setBusy(true);
    setFailure(null);
    try {
      await updateAwsConnection(host.id, name.trim(), region.trim());
      await loadSettings();
      setEditing(false);
    } catch (e) {
      setFailure({ error: toAwsError(e), retry: save });
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    try {
      setAppSettings(await removeHostConnection(host.id));
    } catch (e) {
      setFailure({ error: toAwsError(e), retry: remove });
      setConfirmRemove(false);
      setBusy(false);
    }
  };

  return (
    <li
      ref={rowRef}
      data-testid={`host-${host.id}`}
      className="border border-[var(--border-color)] rounded-[var(--radius-control)] p-2.5 space-y-1.5"
    >
      {editing ? (
        <div className="space-y-2">
          <Field label="Name">{(id) => <input id={id} className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />}</Field>
          <Field label="Default region">
            {(id) => (
              <input id={id} className={inputClass} list="aws-regions" value={region} onChange={(e) => setRegion(e.target.value)} />
            )}
          </Field>
          <div className="flex gap-2">
            <Button size="sm" variant="primary" onClick={save} disabled={busy || !name.trim() || !region.trim()}>
              Save changes
            </Button>
            <Button size="sm" onClick={() => { setEditing(false); setName(host.name); setRegion(host.kind.default_region); }}>
              Cancel
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex items-start justify-between gap-2 flex-wrap">
          <div className="min-w-0">
            <div className="text-[13px] font-medium text-[var(--text-primary)]">{host.name}</div>
            <div className="text-xs text-[var(--text-secondary)]">
              <span>{auth.method === "profile" ? `Profile: ${auth.name}` : "Access keys"}</span>
              {" · "}
              <span>{host.kind.default_region}</span>
            </div>
          </div>
          <div className="flex gap-1.5">
            <Button size="sm" onClick={runCheck} disabled={busy} aria-label={`Check ${host.name}`}>
              Check
            </Button>
            <Button size="sm" onClick={() => setEditing(true)} aria-label={`Edit ${host.name}`}>
              Edit
            </Button>
            {!confirmRemove && (
              <Button data-focus="remove" size="sm" variant="danger" onClick={() => setConfirmRemove(true)} aria-label={`Remove ${host.name}`}>
                Remove
              </Button>
            )}
          </div>
        </div>
      )}
      {confirmRemove && (
        <div role="alertdialog" aria-label={`Remove ${host.name}?`} className="flex items-center gap-2 flex-wrap text-xs">
          <span>Remove {host.name}? Its stored credentials are deleted.</span>
          <Button data-focus="confirm-remove" size="sm" variant="danger" onClick={remove} disabled={busy} aria-label={`Confirm remove ${host.name}`}>
            Confirm remove
          </Button>
          <Button size="sm" onClick={cancelRemove}>
            Cancel
          </Button>
        </div>
      )}
      {busy && !editing && !confirmRemove && <p role="status" className="text-xs text-[var(--text-secondary)]">Working…</p>}
      {failure && <ErrorLine failure={failure} onSignIn={signIn} signingIn={signingIn} />}
      {report && <PermissionReportView report={report} />}
    </li>
  );
}

function AddConnectionForm() {
  const { loadSettings } = useSettings();
  const [profiles, setProfiles] = useState<string[]>([]);
  const [auth, setAuth] = useState<AuthChoice>("profile");
  const [name, setName] = useState("");
  const [profile, setProfile] = useState("");
  const [accessKeyId, setAccessKeyId] = useState("");
  const [secret, setSecret] = useState("");
  const [token, setToken] = useState("");
  const [region, setRegion] = useState("us-east-1");
  const [busy, setBusy] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const [signedInAs, setSignedInAs] = useState<string | null>(null);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [profilesError, setProfilesError] = useState<string | null>(null);
  const [pendingId, setPendingId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listAwsProfiles()
      .then((p) => {
        if (!cancelled) setProfiles(p);
      })
      .catch((e) => {
        if (!cancelled) {
          setProfiles([]);
          setProfilesError(awsErrorMessage(toAwsError(e)));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const ready =
    name.trim() !== "" &&
    region.trim() !== "" &&
    (auth === "profile" ? profile !== "" : accessKeyId.trim() !== "" && secret !== "");

  /** Second step: confirm the new connection can sign in. Retried alone after an SSO login. */
  const verify = async (id: string) => {
    try {
      const who = await awsConnectionIdentity(id);
      setSignedInAs(who.arn);
      setFailure(null);
    } catch (e) {
      setFailure({ error: toAwsError(e), retry: () => verify(id) });
    }
  };

  /** Everything after a successful add. Retrying never re-runs the add. */
  const afterAdd = async (id: string) => {
    const refreshed = await loadSettings();
    if (!refreshed) {
      setFailure({
        error: { kind: "other", message: "The connection was added, but the settings list could not be refreshed." },
        retry: () => afterAdd(id),
      });
      return;
    }
    await verify(id);
  };

  const submit = async () => {
    setBusy(true);
    setFailure(null);
    setSignedInAs(null);
    try {
      setPendingId(null);
      const added =
        auth === "profile"
          ? await addAwsProfileConnection(name.trim(), profile, region.trim())
          : await addAwsKeysConnection(name.trim(), accessKeyId.trim(), secret, token.trim() || null, region.trim());
      if (auth === "keys") {
        setAccessKeyId("");
        setSecret("");
        setToken("");
      }
      setName("");
      setPendingId(added.id);
      await afterAdd(added.id);
    } catch (e) {
      setFailure({ error: toAwsError(e), retry: submit });
    } finally {
      setBusy(false);
    }
  };

  const signIn = async () => {
    if (!failure || !pendingId) return;
    const retry = failure.retry;
    setSigningIn(true);
    try {
      await awsConnectionSsoLogin(pendingId);
      setFailure(null);
      await retry();
    } catch (e) {
      setFailure({ error: toAwsError(e), retry });
    } finally {
      setSigningIn(false);
    }
  };

  return (
    <form
      className="space-y-3"
      onSubmit={(e) => {
        e.preventDefault();
        if (ready && !busy) void submit();
      }}
    >
      <h4 className="text-[13px] font-semibold text-[var(--text-primary)]">Add AWS connection</h4>
      <Field label="Name">
        {(id) => <input id={id} className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />}
      </Field>
      <SegmentedControl<AuthChoice>
        label="Authentication"
        value={auth}
        onChange={setAuth}
        segments={[
          { value: "profile", label: "Profile" },
          { value: "keys", label: "Access keys" },
        ]}
      />
      {auth === "profile" ? (
        <Field
          label="Profile"
          hint={profilesError ? `Couldn't read your AWS profiles: ${profilesError}` : undefined}
        >
          {(id) => (
            <select id={id} className={`${selectClass} w-full`} value={profile} onChange={(e) => setProfile(e.target.value)}>
              <option value="">Select a profile…</option>
              {profiles.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          )}
        </Field>
      ) : (
        <>
          <Field label="Access key ID">
            {(id) => (
              <input id={id} type="password" autoComplete="off" className={inputClass} value={accessKeyId} onChange={(e) => setAccessKeyId(e.target.value)} />
            )}
          </Field>
          <Field label="Secret access key">
            {(id) => (
              <input id={id} type="password" autoComplete="off" className={inputClass} value={secret} onChange={(e) => setSecret(e.target.value)} />
            )}
          </Field>
          <Field label="Session token (optional)" hint="Only for temporary credentials.">
            {(id) => (
              <input id={id} type="password" autoComplete="off" className={inputClass} value={token} onChange={(e) => setToken(e.target.value)} />
            )}
          </Field>
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            Stored in the OS keychain and never shown again.
          </p>
        </>
      )}
      <Field label="Default region">
        {(id) => (
          <input id={id} className={inputClass} list="aws-regions" value={region} onChange={(e) => setRegion(e.target.value)} />
        )}
      </Field>
      <Button type="submit" size="md" variant="primary" disabled={!ready || busy}>
        {busy ? "Adding…" : "Add connection"}
      </Button>
      {signedInAs && (
        <p role="status" className="text-xs text-[var(--success)]">
          Signed in as {signedInAs}
        </p>
      )}
      {failure && (
        <ErrorLine failure={failure} onSignIn={pendingId ? signIn : undefined} signingIn={signingIn} />
      )}
    </form>
  );
}

export default function HostsSettings() {
  const { appSettings } = useSettings();
  const hosts = appSettings?.hosts ?? [];
  return (
    <div className="space-y-3">
      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        Where project environments run. AWS connections are used to check what your credentials can do.
      </p>
      <ul className="space-y-2">
        <li className="border border-[var(--border-color)] rounded-[var(--radius-control)] p-2.5">
          <div className="text-[13px] font-medium text-[var(--text-primary)]">Local Docker</div>
          <div className="text-xs text-[var(--text-secondary)]">Always available</div>
        </li>
        {hosts.map((h) => (
          <ConnectionRow key={h.id} host={h} />
        ))}
      </ul>
      <div className="pt-3 border-t border-[var(--border-color)]">
        <AddConnectionForm />
      </div>
      <datalist id="aws-regions">
        {COMMON_REGIONS.map((r) => (
          <option key={r} value={r} />
        ))}
      </datalist>
    </div>
  );
}
