import { useEffect, useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import SegmentedControl from "../ui/SegmentedControl";
import Field, { inputClass, selectClass } from "../ui/Field";
import {
  addMarketplaceGhHostAccount,
  addMarketplaceTokenAccount,
  getSettings,
  marketplaceGhHostAvailable,
} from "../../lib/tauri-commands";
import { useAppState } from "../../store/appState";
import GhContainerLoginModal from "./GhContainerLoginModal";

type Method = "gh" | "token";

interface Props {
  onClose: () => void;
}

export default function AddAccountModal({ onClose }: Props) {
  const projects = useAppState((s) => s.projects);
  const setAppSettings = useAppState((s) => s.setAppSettings);
  const runnable = projects.filter((p) => p.status === "running" && p.container_id);

  const [method, setMethod] = useState<Method>("gh");
  const [hostGh, setHostGh] = useState<boolean | null>(null);
  const [label, setLabel] = useState("");
  const [host, setHost] = useState("github.com");
  const [token, setToken] = useState("");
  const [projectId, setProjectId] = useState(runnable[0]?.id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [containerLogin, setContainerLogin] = useState(false);

  useEffect(() => {
    let cancelled = false;
    marketplaceGhHostAvailable()
      .then((v) => {
        if (!cancelled) setHostGh(v);
      })
      .catch(() => {
        if (!cancelled) setHostGh(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const reloadSettings = async () => setAppSettings(await getSettings());

  const finish = async () => {
    await reloadSettings();
    onClose();
  };

  const submit = async () => {
    setError(null);
    if (method === "gh" && !hostGh) {
      setContainerLogin(true);
      return;
    }
    setBusy(true);
    try {
      if (method === "gh") {
        await addMarketplaceGhHostAccount(label.trim(), host.trim());
      } else {
        const t = token.trim();
        setToken("");
        await addMarketplaceTokenAccount(label.trim(), host.trim(), t);
      }
      await finish();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  const hostValid = /^[A-Za-z0-9.-]+(:[0-9]+)?$/.test(host.trim());
  const needsContainer = method === "gh" && hostGh === false;
  const canSubmit =
    !busy &&
    hostGh !== null &&
    label.trim() !== "" &&
    hostValid &&
    (method === "gh" ? !needsContainer || projectId !== "" : token.trim() !== "");

  if (containerLogin) {
    const project = runnable.find((p) => p.id === projectId);
    return (
      <GhContainerLoginModal
        label={label.trim()}
        host={host.trim()}
        projectId={projectId}
        projectName={project?.name ?? projectId}
        onClose={onClose}
        onDone={() => void finish()}
      />
    );
  }

  return (
    <Modal
      title="Add account"
      description="Accounts let Triple-C read private marketplace repositories. Credentials stay on this computer and never enter containers."
      widthClassName="w-[36rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={() => void submit()} disabled={!canSubmit}>
            {needsContainer ? "Sign in" : busy ? "Checking…" : "Add account"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <SegmentedControl<Method>
          label="Sign-in method"
          value={method}
          onChange={(m) => {
            setMethod(m);
            setError(null);
          }}
          segments={[
            { value: "gh", label: "GitHub via gh" },
            { value: "token", label: "Access token" },
          ]}
        />
        <Field label="Label">
          {(id) => (
            <input id={id} value={label} onChange={(e) => setLabel(e.target.value)} className={inputClass} placeholder="Work GitHub" />
          )}
        </Field>
        <Field label="Host" hint={hostValid ? undefined : "Host name only, e.g. github.com or repo.example.com"}>
          {(id) => <input id={id} value={host} onChange={(e) => setHost(e.target.value)} className={inputClass} />}
        </Field>
        {method === "gh" && hostGh === true && (
          <p className="text-xs text-[var(--text-secondary)]">
            gh is installed on this computer. Triple-C asks it for a token each time it fetches, so signing out of gh
            also signs this account out. If gh is not logged in yet, run <code className="font-mono">gh auth login</code> first.
          </p>
        )}
        {needsContainer &&
          (runnable.length === 0 ? (
            <p className="text-xs text-[var(--warning)]">
              gh is not installed on this computer. Start a project so gh can run in its container, or use an access token.
            </p>
          ) : (
            <Field
              label="Run gh in"
              hint="gh is not installed on this computer, so the sign-in runs in this container. The token is kept in your OS keychain, not in the container."
            >
              {(id) => (
                <select id={id} value={projectId} onChange={(e) => setProjectId(e.target.value)} className={selectClass}>
                  {runnable.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              )}
            </Field>
          ))}
        {method === "token" && (
          <Field
            label="Token"
            hint="A personal access token with read access to the repository. For GitHub SSO orgs, authorise the token for the org."
          >
            {(id) => (
              <input
                id={id}
                type="password"
                autoComplete="off"
                value={token}
                onChange={(e) => setToken(e.target.value)}
                className={inputClass}
              />
            )}
          </Field>
        )}
        {error && <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap">{error}</p>}
      </div>
    </Modal>
  );
}
