import { useEffect, useRef, useState } from "react";
import type { RepoSource } from "../../lib/types";
import { cleanRepos, repoFolderFromUrl } from "../../lib/awsWorkspace";
import Button from "../ui/Button";
import { inputClass, monoInputClass } from "../ui/Field";

export interface RepoRowsProps {
  rows: RepoSource[];
  onRowsChange: (rows: RepoSource[]) => void;
  /** Called when focus leaves a field (save-on-blur editors). */
  onCommit?: () => void;
  disabled?: boolean;
}

export const emptyRepo = (): RepoSource => ({ url: "", branch: null, folder: "" });

/** Rows of URL / branch / folder. The folder follows the URL until it is edited. */
export function RepoRows({ rows, onRowsChange, onCommit, disabled = false }: RepoRowsProps) {
  const update = (i: number, patch: Partial<RepoSource>) => {
    const next = [...rows];
    next[i] = { ...next[i], ...patch };
    onRowsChange(next);
  };
  return (
    <div className="space-y-2">
      {rows.map((r, i) => (
        <div
          key={i}
          className="space-y-1.5 p-2 bg-[var(--bg-primary)] rounded-[var(--radius-control)] border border-[var(--border-color)]"
        >
          <input
            value={r.url}
            aria-label={`Repository ${i + 1} URL`}
            placeholder="git@github.com:org/repo.git or https://…"
            disabled={disabled}
            onChange={(e) => {
              const url = e.target.value;
              const followed = !r.folder || r.folder === repoFolderFromUrl(r.url);
              update(i, { url, ...(followed ? { folder: repoFolderFromUrl(url) } : {}) });
            }}
            onBlur={onCommit}
            className={monoInputClass}
          />
          <div className="flex items-center gap-1.5">
            <input
              value={r.branch ?? ""}
              aria-label={`Repository ${i + 1} branch`}
              placeholder="default branch"
              disabled={disabled}
              onChange={(e) => update(i, { branch: e.target.value })}
              onBlur={onCommit}
              className={`w-40 ${inputClass}`}
            />
            <span className="text-xs text-[var(--text-secondary)] flex-shrink-0 font-mono">
              /workspace/
            </span>
            <input
              value={r.folder}
              aria-label={`Repository ${i + 1} folder`}
              placeholder="folder"
              disabled={disabled}
              onChange={(e) => update(i, { folder: e.target.value })}
              onBlur={onCommit}
              className={monoInputClass}
            />
            <Button
              size="md"
              variant="danger"
              disabled={disabled}
              aria-label={`Remove repository ${i + 1}`}
              onClick={() => {
                onRowsChange(rows.filter((_, j) => j !== i));
                // A removal is a finished edit; `onRowsChange` has already
                // handed the shorter list over.
                onCommit?.();
              }}
            >
              Remove
            </Button>
          </div>
        </div>
      ))}
      <Button disabled={disabled} onClick={() => onRowsChange([...rows, emptyRepo()])}>
        + Add repository
      </Button>
    </div>
  );
}

function sameRepos(a: RepoSource[], b: RepoSource[]): boolean {
  return (
    a.length === b.length &&
    a.every((r, i) => r.url === b[i].url && r.branch === b[i].branch && r.folder === b[i].folder)
  );
}

/**
 * The Config tab's save-on-blur repository list. Saves when every row that
 * has anything in it has a URL; the backend's validation message (bad URL,
 * folder used twice…) comes back through the save's toast.
 *
 * Rows in progress survive a refresh of the project list: they are replaced
 * from `repos` only when the stored value actually changed.
 */
export default function RepoListEditor({
  repos,
  onSave,
  disabled = false,
}: {
  repos: RepoSource[];
  onSave: (repos: RepoSource[]) => Promise<boolean>;
  disabled?: boolean;
}) {
  const [rows, setRows] = useState<RepoSource[]>(repos);
  // The latest rows, so a commit right after an edit (a removal) sees them.
  const rowsRef = useRef(rows);
  const storedRef = useRef(repos);

  useEffect(() => {
    if (sameRepos(repos, storedRef.current)) return;
    storedRef.current = repos;
    rowsRef.current = repos;
    setRows(repos);
  }, [repos]);

  const change = (next: RepoSource[]) => {
    rowsRef.current = next;
    setRows(next);
  };

  const commit = () => {
    const cleaned = cleanRepos(rowsRef.current);
    if (cleaned.some((r) => !r.url)) return;
    if (sameRepos(cleaned, storedRef.current)) return;
    void onSave(cleaned);
  };

  return <RepoRows rows={rows} onRowsChange={change} onCommit={commit} disabled={disabled} />;
}
