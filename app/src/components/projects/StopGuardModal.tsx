import { useRef } from "react";
import type { UnsavedWork, WipChoice } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";

export type StopGuardChoice = WipChoice | "unguarded" | null;

interface Props {
  projectName: string;
  work: UnsavedWork;
  /** The project's "push work in progress when the workspace stops" setting. */
  wipPushOnStop?: boolean;
  /** `null` = Cancel. `"unguarded"` = stop without recording any choice
   *  (the unchecked case: the workspace's own shutdown push still runs). */
  onChoose: (choice: StopGuardChoice) => void;
}

function describeRepo(dirty: boolean, unpushed: number): string {
  const parts: string[] = [];
  if (dirty) parts.push("uncommitted changes");
  if (unpushed > 0) parts.push(`${unpushed} unpushed commit${unpushed === 1 ? "" : "s"}`);
  return parts.join(", ");
}

/**
 * Asked before an AWS workspace stops when its repositories hold work no
 * remote has, or when Triple-C could not check. Pushing to a WIP branch is
 * the default (it has focus); it is not offered when the check could not
 * run, since the push goes the same way the check did.
 */
export default function StopGuardModal({ projectName, work, wipPushOnStop = false, onChoose }: Props) {
  const pushRef = useRef<HTMLButtonElement>(null);
  const stopRef = useRef<HTMLButtonElement>(null);
  const atRisk = work.repos.filter((r) => r.dirty || r.unpushed > 0);

  return (
    <Modal
      title="Unsaved work"
      onClose={() => onChoose(null)}
      widthClassName="w-[30rem]"
      initialFocusRef={work.checked ? pushRef : stopRef}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={() => onChoose(null)}>
            Cancel
          </Button>
          <Button
            ref={stopRef}
            size="md"
            variant={work.checked ? "danger" : "primary"}
            onClick={() => onChoose(work.checked ? "discard" : "unguarded")}
          >
            {work.checked ? "Stop without pushing" : "Stop anyway"}
          </Button>
          {work.checked && (
            <Button ref={pushRef} size="md" variant="primary" onClick={() => onChoose("push")}>
              Push to WIP branch and stop
            </Button>
          )}
        </>
      }
    >
      {work.checked ? (
        <>
          <p className="text-[13px] text-[var(--text-secondary)]">
            Stopping <strong className="text-[var(--text-primary)]">{projectName}</strong> ends
            its workspace. These repositories have work that no remote has yet:
          </p>
          <ul className="mt-2 space-y-1">
            {atRisk.map((r) => (
              <li key={r.folder} className="text-[13px] text-[var(--text-primary)]">
                <span className="font-mono">/workspace/{r.folder}</span>{" "}
                <span className="text-[var(--text-secondary)]">
                  ({r.branch}): {describeRepo(r.dirty, r.unpushed)}
                </span>
              </li>
            ))}
          </ul>
          <p className="mt-3 text-[13px] text-[var(--text-secondary)]">
            Push to WIP branch saves a snapshot of each repository to a{" "}
            <span className="font-mono">triple-c/wip/…</span> branch on its remote, without
            touching your branches. If the push fails, the workspace keeps running. Stop
            without pushing skips that push, including the one the workspace would make as it
            shuts down.
          </p>
        </>
      ) : (
        <>
          <p className="text-[13px] text-[var(--text-secondary)]">
            Triple-C could not check{" "}
            <strong className="text-[var(--text-primary)]">{projectName}</strong> for unsaved
            work, so it can&rsquo;t push it to a WIP branch first.
          </p>
          {work.error && (
            <p
              role="note"
              className="mt-2 px-2 py-1.5 text-xs text-[var(--warning)] bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)]"
            >
              {work.error}
            </p>
          )}
          <p className="mt-2 text-[13px] text-[var(--text-secondary)]">
            Uncommitted or unpushed work in the workspace may be lost if you stop it now.
            {wipPushOnStop &&
              " The workspace's own push of work in progress still runs as it shuts down, if it can reach the remote."}
          </p>
        </>
      )}
    </Modal>
  );
}
