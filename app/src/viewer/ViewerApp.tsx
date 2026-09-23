import { useEffect, useState } from "react";
import Button from "../components/ui/Button";
import { viewerChooseFile, viewerGetState } from "../lib/tauri-commands";
import type { ViewerState } from "../lib/types";
import EditorPane from "./EditorPane";

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export default function ViewerApp() {
  const [state, setState] = useState<ViewerState | { error: string } | null>(null);
  const [chooseError, setChooseError] = useState<string | null>(null);

  useEffect(() => {
    viewerGetState().then(setState, (e) => setState({ error: errorText(e) }));
  }, []);

  if (state === null) return <p className="p-4 text-sm text-[var(--text-secondary)]">Loading…</p>;
  if ("error" in state) return <p className="p-4 text-sm">{state.error}</p>;

  const choose = (index: number) => {
    setChooseError(null);
    viewerChooseFile(index).then(setState, (e) => setChooseError(errorText(e)));
  };

  switch (state.state.kind) {
    case "resolved":
      return <EditorPane state={state} />;
    case "not_found":
      return (
        <div className="p-4 text-sm">
          <p>Could not find <span className="font-mono">{state.raw_path}</span> in the container. Looked in:</p>
          <ul className="mt-2 list-disc pl-6 font-mono text-xs text-[var(--text-secondary)]">
            {state.state.tried.map((p) => <li key={p}>{p}</li>)}
          </ul>
        </div>
      );
    case "choose":
      return (
        <div className="p-4 text-sm">
          <p>Several files match <span className="font-mono">{state.raw_path}</span>. Open which?</p>
          <ul className="mt-2 flex flex-col items-start gap-1">
            {state.state.candidates.map((p, i) => (
              <li key={p}>
                <Button size="sm" onClick={() => choose(i)}>
                  <span className="font-mono">{p}</span>
                </Button>
              </li>
            ))}
          </ul>
          {chooseError && <p role="alert" className="mt-2">{chooseError}</p>}
        </div>
      );
  }
}
