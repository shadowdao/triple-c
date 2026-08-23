import { useId, useRef, useState, type ReactNode } from "react";
import Modal from "./Modal";
import Button from "./Button";
import { inputClass } from "./Field";

interface Props {
  title: string;
  /** What must be typed, verbatim, before the confirm button enables. */
  expected: string;
  /** The verb on the confirm button. Repeat the action — never "OK". */
  confirmLabel: string;
  /** What is about to be lost, in full. */
  children: ReactNode;
  onConfirm: (typed: string) => void;
  onCancel: () => void;
  busy?: boolean;
}

/**
 * The confirmation gate for something that has no other copy.
 *
 * ## Why this exists when `ConfirmResetModal` already did
 *
 * Reset and Remove are reached from a project's own overflow menu, one project
 * at a time, by a user who went looking for them. The Disk panel lists every
 * project's volumes side by side in a table of numbers, sorted by size — which
 * is exactly the layout that invites a misclick on the wrong row. A two-button
 * dialog does not survive that, because the thing being confirmed (*which*
 * project) is the thing the user got wrong.
 *
 * Typing the name fixes the failure mode rather than adding friction to it: the
 * gate is not "are you sure", it is "name the project you mean".
 *
 * The comparison is `expected.trim() === typed.trim()` and **case-sensitive** —
 * mirroring `confirmation_matches` in `docker/disk.rs`, which is the check that
 * actually holds, since this one is only a UI affordance. The backend refuses a
 * mismatch on its own.
 */
export default function TypedConfirmModal({
  title,
  expected,
  confirmLabel,
  children,
  onConfirm,
  onCancel,
  busy = false,
}: Props) {
  const [typed, setTyped] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  // Every other `ui/` component uses `useId`; a hardcoded id breaks the
  // label association as soon as two of these are mounted at once.
  const inputId = useId();
  const matches = expected.trim().length > 0 && typed.trim() === expected.trim();

  return (
    <Modal
      title={title}
      onClose={onCancel}
      widthClassName="w-[30rem]"
      initialFocusRef={inputRef}
      dismissible={!busy}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            size="md"
            onClick={() => onConfirm(typed)}
            disabled={!matches || busy}
            className={
              matches && !busy
                ? "bg-[var(--error-emphasis)] text-white border border-transparent hover:opacity-90"
                : "bg-[var(--bg-tertiary)] text-[var(--text-disabled)] border border-[var(--border-color)]"
            }
          >
            {busy ? "Working…" : confirmLabel}
          </Button>
        </>
      }
    >
      <div className="space-y-3 text-[13px] text-[var(--text-secondary)]">
        {children}
        <div>
          <label
            htmlFor={inputId}
            className="block text-[13px] text-[var(--text-primary)] mb-1.5"
          >
            Type <strong className="font-mono">{expected}</strong> to confirm
          </label>
          <input
            id={inputId}
            ref={inputRef}
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            disabled={busy}
            autoComplete="off"
            spellCheck={false}
            className={`${inputClass} font-mono`}
          />
          {/* Announced rather than only coloured — the gate's state has to be
              readable without relying on the button's fill. */}
          <p role="status" aria-live="polite" className="mt-1.5 text-xs">
            {matches ? (
              <span className="text-[var(--text-secondary)]">Name matches.</span>
            ) : (
              <span className="text-[var(--text-disabled)]">
                Waiting for the exact project name.
              </span>
            )}
          </p>
        </div>
      </div>
    </Modal>
  );
}
