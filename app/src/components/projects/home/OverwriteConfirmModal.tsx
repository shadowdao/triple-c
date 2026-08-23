import type { OverwriteChoice } from "../../../lib/uploadErrors";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";

interface Props {
  /** Bare name of the file that is already there. */
  name: string;
  /** Container directory it is going into. */
  directory: string;
  /** How many more files are queued behind this one. */
  remaining: number;
  onChoose: (choice: OverwriteChoice) => void;
}

/**
 * "That name is taken — replace it?"
 *
 * This exists because the backend stopped overwriting silently, and a raw
 * error string would have been a worse answer than the old silent clobber: it
 * tells the user their drop failed without telling them it *can* succeed. The
 * dialog names the file and the directory, because a drop is aimed with a
 * mouse and "notes.txt" alone does not say which `notes.txt`.
 *
 * The blanket answers only appear when there is something to apply them to — a
 * single-file drop with "Replace all" on it invites the reflex of clicking the
 * widest button for no benefit.
 *
 * Dismissing (Escape, ✕, click-outside) is a **skip**, never a replace: the
 * destructive answer has to be chosen explicitly.
 */
export default function OverwriteConfirmModal({ name, directory, remaining, onChoose }: Props) {
  const footer = (
    <>
      {remaining > 0 && (
        <>
          <Button size="md" onClick={() => onChoose("skip-all")}>
            Skip all
          </Button>
          <Button size="md" onClick={() => onChoose("replace-all")}>
            Replace all
          </Button>
        </>
      )}
      <Button size="md" onClick={() => onChoose("skip")}>
        Skip
      </Button>
      <Button size="md" variant="primary" onClick={() => onChoose("replace")}>
        Replace
      </Button>
    </>
  );

  return (
    <Modal
      title="A file with that name is already there"
      description={directory}
      onClose={() => onChoose("skip")}
      footer={footer}
      widthClassName="w-[30rem]"
    >
      <p className="text-[13px] text-[var(--text-primary)]">
        <span className="font-mono">{name}</span> already exists in{" "}
        <span className="font-mono">{directory}</span>. Replacing it overwrites the container's
        copy, and that cannot be undone from here.
      </p>
      {remaining > 0 && (
        <p className="mt-2 text-xs text-[var(--text-secondary)]">
          {remaining} more file{remaining === 1 ? "" : "s"} still to upload.
        </p>
      )}
    </Modal>
  );
}
