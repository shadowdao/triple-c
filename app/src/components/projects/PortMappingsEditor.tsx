import { useEffect, useRef, useState } from "react";
import type { PortMapping } from "../../lib/types";
import Button from "../ui/Button";
import { monoInputClass, selectClass } from "../ui/Field";

interface Props {
  portMappings: PortMapping[];
  disabled: boolean;
  disabledReason?: string;
  /** AWS: only TCP is forwarded (UDP is refused on save). */
  tcpOnly?: boolean;
  onSave: (mappings: PortMapping[]) => Promise<unknown>;
}

/** One row as typed: ports stay text until they are valid, so a half-typed
 *  row is never turned into a 0. */
interface Row {
  key: number;
  host: string;
  container: string;
  protocol: string;
}

/** Placeholders are words, not port numbers: a "8080" placeholder in an
 *  empty row read as a filled-in 8080 → 8080 mapping that was never typed. */
const placeholderClass = "placeholder:italic placeholder:text-[var(--text-disabled)]";

function parsePort(text: string): number | null {
  if (!/^\d+$/.test(text.trim())) return null;
  const n = Number(text.trim());
  return n >= 1 && n <= 65535 ? n : null;
}

/** Why a row is not a mapping yet, or `null` when it is one. */
export function rowProblem(row: Pick<Row, "host" | "container">): string | null {
  if (row.host.trim() === "" || row.container.trim() === "") return "Fill in both ports";
  if (parsePort(row.host) === null || parsePort(row.container) === null) {
    return "Ports must be whole numbers between 1 and 65535";
  }
  return null;
}

/** The rows that are complete mappings — the only ones ever saved. */
function completeMappings(rows: Row[]): PortMapping[] {
  return rows.flatMap((r) => {
    const host_port = parsePort(r.host);
    const container_port = parsePort(r.container);
    return host_port !== null && container_port !== null && rowProblem(r) === null
      ? [{ host_port, container_port, protocol: r.protocol }]
      : [];
  });
}

let nextKey = 1;
const portText = (n: number) => (n > 0 ? String(n) : "");
function toRows(mappings: PortMapping[]): Row[] {
  return mappings.map((m) => ({
    key: nextKey++,
    host: portText(m.host_port),
    container: portText(m.container_port),
    protocol: m.protocol,
  }));
}

export default function PortMappingsEditor({
  portMappings: stored,
  disabled,
  disabledReason,
  tcpOnly = false,
  onSave,
}: Props) {
  const [rows, setRowsState] = useState<Row[]>(() => toRows(stored));
  // The latest rows, for blur handlers that run before a re-render.
  const rowsRef = useRef(rows);
  const setRows = (next: Row[]) => {
    rowsRef.current = next;
    setRowsState(next);
  };

  // What is stored, as this editor last loaded or saved it. A project refresh
  // hands over a new array on every update (status events, save responses,
  // list reloads); only a value that differs from this is a real change.
  const storedKey = JSON.stringify(stored);
  const syncedKey = useRef(storedKey);
  const wrapper = useRef<HTMLDivElement>(null);
  const [focusLeft, setFocusLeft] = useState(0);
  const focusHost = useRef<number | null>(null);

  useEffect(() => {
    if (storedKey === syncedKey.current) return;
    // Never replace what the user is typing; this runs again when focus
    // leaves the editor.
    if (wrapper.current?.contains(document.activeElement)) return;
    syncedKey.current = storedKey;
    const fresh = toRows(JSON.parse(storedKey) as PortMapping[]);
    // Rows still being filled in are not stored; keep them.
    const drafts = rowsRef.current.filter((r) => rowProblem(r) !== null);
    const freshKeys = new Set(fresh.map((r) => `${r.host}|${r.container}`));
    setRows([...fresh, ...drafts.filter((r) => !freshKeys.has(`${r.host}|${r.container}`))]);
  }, [storedKey, focusLeft]);

  useEffect(() => {
    if (focusHost.current === null) return;
    const el = wrapper.current?.querySelector<HTMLInputElement>(
      `[data-row="${focusHost.current}"][data-field="host"]`,
    );
    focusHost.current = null;
    el?.focus();
  }, [rows]);

  /** Save the complete rows, if they differ from what is stored. */
  const commit = (next: Row[]) => {
    const mappings = completeMappings(next);
    const key = JSON.stringify(mappings);
    if (key === syncedKey.current) return;
    syncedKey.current = key;
    void onSave(mappings);
  };

  const edit = (index: number, field: "host" | "container", value: string) => {
    const next = [...rowsRef.current];
    next[index] = { ...next[index], [field]: value };
    setRows(next);
  };

  const portInput = (row: Row, i: number, field: "host" | "container") => {
    const value = row[field];
    const missing = rowProblem(row) !== null && parsePort(value) === null;
    return (
      <input
        type="text"
        inputMode="numeric"
        value={value}
        onChange={(e) => edit(i, field, e.target.value)}
        onBlur={() => commit(rowsRef.current)}
        placeholder={field === "host" ? "Host port" : "Container port"}
        aria-label={`${field === "host" ? "Host" : "Container"} port ${i + 1}`}
        aria-invalid={missing || undefined}
        aria-describedby={rowProblem(row) ? `pm-problem-${row.key}` : undefined}
        data-row={row.key}
        data-field={field}
        disabled={disabled}
        className={`w-[28%] ${monoInputClass} ${placeholderClass}`}
      />
    );
  };

  return (
    <div
      ref={wrapper}
      className="space-y-2"
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocusLeft((n) => n + 1);
      }}
    >
      {disabled && disabledReason && (
        <p className="px-2 py-1.5 bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)] text-xs text-[var(--warning)]">
          {disabledReason}
        </p>
      )}

      {rows.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">No port mappings configured.</p>
      )}

      {rows.length > 0 && (
        <div className="flex gap-2 items-center text-xs text-[var(--text-secondary)] px-0.5">
          <span className="w-[28%]">Host port</span>
          <span className="w-[28%]">Container port</span>
          <span className="w-[22%]">Protocol</span>
          <span className="flex-1" />
        </div>
      )}

      {rows.map((row, i) => {
        const problem = rowProblem(row);
        return (
          <div key={row.key} className="space-y-1">
            <div className="flex gap-2 items-center">
              {portInput(row, i, "host")}
              {portInput(row, i, "container")}
              <select
                value={row.protocol}
                aria-label={`Protocol ${i + 1}`}
                onChange={(e) => {
                  const next = [...rowsRef.current];
                  next[i] = { ...next[i], protocol: e.target.value };
                  setRows(next);
                  commit(next);
                }}
                disabled={disabled}
                className={`w-[22%] ${selectClass}`}
              >
                <option value="tcp">TCP</option>
                {(!tcpOnly || row.protocol === "udp") && <option value="udp">UDP</option>}
              </select>
              <Button
                variant="danger"
                disabled={disabled}
                aria-label={`Remove port mapping ${i + 1}`}
                onClick={() => {
                  const next = rowsRef.current.filter((_, j) => j !== i);
                  setRows(next);
                  commit(next);
                }}
              >
                Remove
              </Button>
            </div>
            {problem && (
              <p id={`pm-problem-${row.key}`} className="text-xs text-[var(--warning)] px-0.5">
                ⚠ {problem} — this mapping isn't saved or active.
              </p>
            )}
          </div>
        );
      })}

      <Button
        disabled={disabled}
        onClick={() => {
          // Kept here until both ports are filled in: an empty row is never
          // saved (it used to be stored as 0 → 0 and silently skipped).
          const row: Row = { key: nextKey++, host: "", container: "", protocol: "tcp" };
          focusHost.current = row.key;
          setRows([...rowsRef.current, row]);
        }}
      >
        + Add port mapping
      </Button>
    </div>
  );
}
