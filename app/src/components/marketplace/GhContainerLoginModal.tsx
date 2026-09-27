import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import StatusIndicator from "../ui/StatusIndicator";
import {
  cancelMarketplaceGhLogin,
  openUrlExternal,
  startMarketplaceGhContainerLogin,
} from "../../lib/tauri-commands";
import type { MarketplaceAccount } from "../../lib/types";

interface Props {
  label: string;
  host: string;
  projectId: string;
  projectName: string;
  onClose: () => void;
  onDone: (account: MarketplaceAccount) => void;
}

interface CodeEvent {
  account_id: string;
  code: string;
  url: string;
}
interface OutputEvent {
  account_id: string;
  chunk: string;
}

const MAX_OUTPUT = 8000;

/** Only open device-login pages on the host being signed in to. */
function safeDeviceUrl(url: string, host: string): string | null {
  try {
    const u = new URL(url);
    return u.protocol === "https:" && u.hostname === host ? u.toString() : null;
  } catch {
    return null;
  }
}

/**
 * Drives `gh auth login --web` inside a running container. The command only
 * resolves when the login finishes, so the new account's id is unknown while it
 * runs; the modal accepts every gh-login event while open. The backend allows
 * one gh login at a time, so there is never another flow's event to confuse.
 */
export default function GhContainerLoginModal({ label, host, projectId, projectName, onClose, onDone }: Props) {
  const [code, setCode] = useState<string | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [output, setOutput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(true);
  const started = useRef(false);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: UnlistenFn[] = [];
    const register = async <T,>(name: string, handle: (p: T) => void) => {
      const un = await listen<T>(name, (e) => handle(e.payload));
      if (cancelled) un();
      else unlisteners.push(un);
    };

    void (async () => {
      await register<CodeEvent>("marketplace-gh-login-code", (p) => {
        setCode(p.code);
        setUrl(p.url);
      });
      await register<OutputEvent>("marketplace-gh-login-output", (p) =>
        setOutput((prev) => {
          const next = prev + p.chunk;
          return next.length > MAX_OUTPUT ? next.slice(next.length - MAX_OUTPUT) : next;
        }),
      );
      if (cancelled || started.current) return;
      started.current = true;
      try {
        const account = await startMarketplaceGhContainerLogin(label, host, projectId);
        if (!cancelled) {
          setRunning(false);
          onDone(account);
        }
      } catch (e) {
        if (!cancelled) {
          setRunning(false);
          setError(typeof e === "string" ? e : String(e));
        }
      }
    })();

    return () => {
      cancelled = true;
      for (const un of unlisteners) {
        try {
          un();
        } catch {
          /* already gone */
        }
      }
    };
    // Runs once per modal instance; the props do not change while it is open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const cancel = () => {
    void cancelMarketplaceGhLogin();
    onClose();
  };

  const openable = url ? safeDeviceUrl(url, host) : null;

  return (
    <Modal
      title={`Sign in to ${host} with gh`}
      description={`Running gh auth login in "${projectName}". The sign-in is not kept in that container.`}
      widthClassName="w-[40rem]"
      dismissible={!running}
      onClose={running ? cancel : onClose}
      footer={
        running ? (
          <Button size="md" variant="ghost" onClick={cancel}>
            Cancel sign-in
          </Button>
        ) : (
          <Button size="md" onClick={onClose}>
            Close
          </Button>
        )
      }
    >
      <div className="space-y-3">
        {running && !code && <StatusIndicator tone="busy" label="Starting gh…" className="text-xs" />}
        {code && running && (
          <div className="space-y-2">
            <p className="text-xs">Enter this code on the GitHub device page:</p>
            <p className="font-mono text-lg tracking-widest select-all">{code}</p>
            {openable ? (
              <Button size="md" variant="primary" onClick={() => void openUrlExternal(openable)}>
                Open GitHub
              </Button>
            ) : (
              url && <p className="text-xs text-[var(--error)]">The sign-in URL did not point at {host}; not opening it.</p>
            )}
          </div>
        )}
        {error && <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap">{error}</p>}
        {output && (
          <pre className="max-h-40 overflow-auto p-2 text-[11px] font-mono whitespace-pre-wrap rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
            {output}
          </pre>
        )}
      </div>
    </Modal>
  );
}
