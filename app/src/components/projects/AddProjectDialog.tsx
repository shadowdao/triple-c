import { useEffect, useId, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useProjects } from "../../hooks/useProjects";
import type { DetectedEnvironment, HostConnection, ProjectPath, RepoSource } from "../../lib/types";
import { detectAwsEnvironments } from "../../lib/tauri-commands";
import { awsErrorMessage, toAwsError } from "../../lib/aws-error";
import { cleanRepos, defaultAwsWorkspace, environmentSelectable } from "../../lib/awsWorkspace";
import { useAppState } from "../../store/appState";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import { inputClass, monoInputClass, selectClass } from "../ui/Field";
import { RepoRows, emptyRepo } from "./RepoListEditor";

interface Props {
  onClose: () => void;
}

interface PathEntry {
  host_path: string;
  mount_name: string;
}

/** Why an environment can't take a new project, for its disabled option. */
function unavailableReason(env: DetectedEnvironment): string {
  const st = env.state.state;
  if (st !== "ready" && st !== "update_available") return st.replace(/_/g, " ");
  return "you have not joined it";
}

type Scan =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "done"; environments: DetectedEnvironment[] };

function basenameFromPath(p: string): string {
  return p.replace(/[/\\]$/, "").split(/[/\\]/).pop() || "";
}

/** Stable, so the store selector does not return a new array each render. */
const NO_HOSTS: HostConnection[] = [];

export default function AddProjectDialog({ onClose }: Props) {
  const { add } = useProjects();
  const hosts = useAppState((s) => s.appSettings?.hosts ?? NO_HOSTS);
  const [hostId, setHostId] = useState<string>("");
  const connection = hosts.find((h) => h.id === hostId) ?? null;
  const [region, setRegion] = useState("");
  const [scanRegion, setScanRegion] = useState("");
  const [scan, setScan] = useState<Scan>({ status: "idle" });
  const [environment, setEnvironment] = useState("");
  const [repos, setRepos] = useState<RepoSource[]>([emptyRepo()]);
  const [name, setName] = useState("");
  const [pathEntries, setPathEntries] = useState<PathEntry[]>([
    { host_path: "", mount_name: "" },
  ]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const formId = useId();

  // Look for environments when the connection or the region (on blur) changes.
  useEffect(() => {
    if (!connection || !scanRegion) {
      setScan({ status: "idle" });
      return;
    }
    let cancelled = false;
    setScan({ status: "loading" });
    detectAwsEnvironments(connection.id, scanRegion).then(
      (found) => {
        if (cancelled) return;
        setScan({ status: "done", environments: found.environments });
        const first = found.environments.find(environmentSelectable);
        setEnvironment(first?.name ?? "");
      },
      (e) => {
        if (!cancelled) setScan({ status: "error", message: awsErrorMessage(toAwsError(e)) });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [connection, scanRegion]);

  const chooseHost = (id: string) => {
    setHostId(id);
    setEnvironment("");
    const conn = hosts.find((h) => h.id === id);
    const r = conn?.kind.default_region ?? "";
    setRegion(r);
    setScanRegion(r);
  };

  const handleBrowse = async (index: number) => {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      const basename = basenameFromPath(selected);
      const entries = [...pathEntries];
      entries[index] = {
        host_path: selected,
        mount_name: entries[index].mount_name || basename,
      };
      setPathEntries(entries);
      // Auto-fill project name from first folder
      if (!name && index === 0) {
        setName(basename);
      }
    }
  };

  const updateEntry = (index: number, field: keyof PathEntry, value: string) => {
    const entries = [...pathEntries];
    entries[index] = { ...entries[index], [field]: value };
    setPathEntries(entries);
  };

  const handleSubmit = async (e?: React.FormEvent) => {
    if (e) e.preventDefault();
    // The submit button is `aria-disabled` rather than `disabled` while an add
    // is in flight, and Enter inside a text field submits the form without
    // touching the button at all. Both routes end here, so the guard does too.
    if (loading) return;
    if (!name.trim()) {
      setError("Project name is required");
      return;
    }
    if (connection) {
      if (!environment) {
        setError("Choose an environment to run the project in");
        return;
      }
      const cleaned = cleanRepos(repos);
      if (cleaned.some((r) => !r.url)) {
        setError("Every repository needs a URL");
        return;
      }
      setLoading(true);
      setError(null);
      try {
        await add(name.trim(), [], {
          hostId: connection.id,
          awsPlacement: { region: scanRegion, environment },
          aws: { ...defaultAwsWorkspace(), repos: cleaned },
        });
        onClose();
      } catch (err) {
        // The backend's validation message (repository URL, folder, …).
        setError(String(err));
      } finally {
        setLoading(false);
      }
      return;
    }
    const validPaths: ProjectPath[] = pathEntries
      .filter((p) => p.host_path.trim())
      .map((p) => ({
        host_path: p.host_path.trim(),
        mount_name: p.mount_name.trim() || basenameFromPath(p.host_path),
      }));
    if (validPaths.length === 0) {
      setError("At least one folder path is required");
      return;
    }
    const mountNames = validPaths.map((p) => p.mount_name);
    if (new Set(mountNames).size !== mountNames.length) {
      setError("Mount names must be unique");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      await add(name.trim(), validPaths);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  return (
    <Modal
      title="Add Project"
      onClose={onClose}
      widthClassName="w-[30rem]"
      initialFocusRef={nameInputRef}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="md"
            variant="primary"
            type="submit"
            form={formId}
            unavailable={loading}
            unavailableReason="The project is being added. Wait for it to finish."
            title={
              loading
                ? "The project is being added. Wait for it to finish."
                : undefined
            }
          >
            {loading ? "Adding…" : "Add Project"}
          </Button>
        </>
      }
    >
      <form id={formId} onSubmit={handleSubmit} className="space-y-4">
        <div>
          <label
            htmlFor={`${formId}-name`}
            className="block text-[13px] font-medium mb-1"
          >
            Project name
          </label>
          <input
            id={`${formId}-name`}
            ref={nameInputRef}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="my-project"
            className={inputClass}
          />
        </div>

        {hosts.length > 0 && (
          <div>
            <label
              htmlFor={`${formId}-host`}
              className="block text-[13px] font-medium mb-1"
            >
              Runs on
            </label>
            <select
              id={`${formId}-host`}
              value={hostId}
              onChange={(e) => chooseHost(e.target.value)}
              className={`w-full ${selectClass}`}
            >
              <option value="">This machine (Docker)</option>
              {hosts.map((h) => (
                <option key={h.id} value={h.id}>
                  {h.name} (AWS)
                </option>
              ))}
            </select>
          </div>
        )}

        {connection ? (
          <>
            <div>
              <label
                htmlFor={`${formId}-region`}
                className="block text-[13px] font-medium mb-1"
              >
                Region
              </label>
              <input
                id={`${formId}-region`}
                value={region}
                onChange={(e) => setRegion(e.target.value)}
                onBlur={() => setScanRegion(region.trim())}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    setScanRegion(region.trim());
                  }
                }}
                className={monoInputClass}
              />
            </div>
            <div>
              <label
                htmlFor={`${formId}-env`}
                className="block text-[13px] font-medium mb-1"
              >
                Environment
              </label>
              {scan.status === "loading" && (
                <p className="text-xs text-[var(--text-secondary)]">
                  Looking for environments in {scanRegion}…
                </p>
              )}
              {scan.status === "error" && (
                <p className="text-xs text-[var(--error)]">{scan.message}</p>
              )}
              {scan.status === "done" && (
                <>
                  {scan.environments.some(environmentSelectable) ? (
                    <select
                      id={`${formId}-env`}
                      value={environment}
                      onChange={(e) => setEnvironment(e.target.value)}
                      className={`w-full ${selectClass}`}
                    >
                      {scan.environments.map((env) => (
                        <option
                          key={env.name}
                          value={env.name}
                          disabled={!environmentSelectable(env)}
                        >
                          {environmentSelectable(env)
                            ? env.name
                            : `${env.name} — ${unavailableReason(env)}`}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <p className="text-xs text-[var(--text-secondary)]">
                      No environment in {scanRegion} is ready for you to use. Deploy or join
                      one in Settings → Hosts.
                    </p>
                  )}
                </>
              )}
            </div>
            <div>
              <span className="block text-[13px] font-medium mb-1">Repositories</span>
              <p className="mb-1.5 text-xs text-[var(--text-secondary)]">
                Cloned into <span className="font-mono">/workspace/&lt;folder&gt;</span> when
                the workspace starts. Memory, storage and the rest are in the project&rsquo;s
                Config tab.
              </p>
              <RepoRows rows={repos} onRowsChange={setRepos} />
            </div>
          </>
        ) : (
          <div>
            <span className="block text-[13px] font-medium mb-1">Folders</span>
            <div className="space-y-2">
              {pathEntries.map((entry, i) => (
                <div
                  key={i}
                  className="space-y-1.5 p-2 bg-[var(--bg-primary)] rounded-[var(--radius-control)] border border-[var(--border-color)]"
                >
                  <div className="flex gap-1.5">
                    <input
                      value={entry.host_path}
                      onChange={(e) => updateEntry(i, "host_path", e.target.value)}
                      placeholder="/path/to/folder"
                      aria-label={`Folder ${i + 1} host path`}
                      className={inputClass}
                    />
                    <Button size="md" onClick={() => handleBrowse(i)}>
                      Browse
                    </Button>
                    {pathEntries.length > 1 && (
                      <Button
                        size="md"
                        variant="danger"
                        aria-label={`Remove folder ${i + 1}`}
                        onClick={() =>
                          setPathEntries(pathEntries.filter((_, j) => j !== i))
                        }
                      >
                        Remove
                      </Button>
                    )}
                  </div>
                  <div className="flex items-center gap-1.5">
                    <span className="text-xs text-[var(--text-secondary)] flex-shrink-0 font-mono">
                      /workspace/
                    </span>
                    <input
                      value={entry.mount_name}
                      onChange={(e) => updateEntry(i, "mount_name", e.target.value)}
                      placeholder="mount-name"
                      aria-label={`Folder ${i + 1} mount name`}
                      className={monoInputClass}
                    />
                  </div>
                </div>
              ))}
            </div>
            <Button
              className="mt-2"
              onClick={() =>
                setPathEntries([...pathEntries, { host_path: "", mount_name: "" }])
              }
            >
              + Add folder
            </Button>
          </div>
        )}

        {error && (
          <div
            role="alert"
            className="px-2 py-1.5 text-xs text-[var(--error)] bg-[var(--error-muted)] border border-[var(--error)]/30 rounded-[var(--radius-control)]"
          >
            {error}
          </div>
        )}
      </form>
    </Modal>
  );
}
