import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import { Annotation, EditorState, Compartment, EditorSelection, type Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, highlightSpecialChars } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { search, searchKeymap } from "@codemirror/search";
import { bracketMatching, indentOnInput } from "@codemirror/language";
import type { ViewerLocation } from "../lib/types";
import { viewerTheme } from "./viewerTheme";
import { highlightExtension, setHighlight } from "./highlightLine";

export interface CodeEditorHandle {
  getDoc(): string;
  /** Replace the whole document, keeping scroll and a clamped cursor. Does not mark dirty. */
  setDoc(text: string): void;
  goTo(loc: ViewerLocation): void;
  focus(): void;
}

export interface CodeEditorProps {
  initialDoc: string;
  readOnly: boolean;
  language: Extension | null;
  lineWrapping: boolean;
  initialLocation: ViewerLocation;
  onDocChanged(): void;
  onSave(): void;
}

/** A `dispatch` from `setDoc` is a reload, not a user edit; the listener must not mark it dirty. */
const reloadTag = Annotation.define<boolean>();

function readOnlyExt(readOnly: boolean): Extension[] {
  return [EditorState.readOnly.of(readOnly), EditorView.editable.of(!readOnly)];
}

export const CodeEditor = forwardRef<CodeEditorHandle, CodeEditorProps>(function CodeEditor(props, ref) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const readOnlyCompartment = useRef(new Compartment());
  const languageCompartment = useRef(new Compartment());
  const wrapCompartment = useRef(new Compartment());
  const callbacks = useRef(props);
  callbacks.current = props;

  useEffect(() => {
    if (!host.current) return;
    const v = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: props.initialDoc,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          drawSelection(),
          history(),
          bracketMatching(),
          indentOnInput(),
          search({ top: true }),
          highlightExtension(),
          viewerTheme,
          keymap.of([
            { key: "Mod-s", run: () => { callbacks.current.onSave(); return true; } },
            ...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab,
          ]),
          readOnlyCompartment.current.of(readOnlyExt(props.readOnly)),
          languageCompartment.current.of(props.language ?? []),
          wrapCompartment.current.of(props.lineWrapping ? EditorView.lineWrapping : []),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !u.transactions.some((tr) => tr.annotation(reloadTag))) callbacks.current.onDocChanged();
          }),
        ],
      }),
    });
    view.current = v;
    goTo(v, props.initialLocation);
    return () => { v.destroy(); view.current = null; };
    // The editor is created once per mount; later prop changes go through compartments below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    view.current?.dispatch({ effects: readOnlyCompartment.current.reconfigure(readOnlyExt(props.readOnly)) });
  }, [props.readOnly]);
  useEffect(() => {
    view.current?.dispatch({ effects: languageCompartment.current.reconfigure(props.language ?? []) });
  }, [props.language]);
  useEffect(() => {
    view.current?.dispatch({ effects: wrapCompartment.current.reconfigure(props.lineWrapping ? EditorView.lineWrapping : []) });
  }, [props.lineWrapping]);

  useImperativeHandle(ref, () => ({
    getDoc: () => view.current?.state.doc.toString() ?? "",
    setDoc: (text) => {
      const v = view.current;
      if (!v) return;
      const scrollTop = v.scrollDOM.scrollTop;
      const head = Math.min(v.state.selection.main.head, text.length);
      v.dispatch({
        changes: { from: 0, to: v.state.doc.length, insert: text },
        selection: EditorSelection.single(head),
        annotations: reloadTag.of(true),
      });
      v.scrollDOM.scrollTop = scrollTop;
    },
    goTo: (loc) => { if (view.current) goTo(view.current, loc); },
    focus: () => view.current?.focus(),
  }));

  return <div ref={host} className="h-full min-h-0" data-testid="code-editor" />;
});

function goTo(v: EditorView, loc: ViewerLocation): void {
  if (loc.line === null) return;
  const from = loc.line;
  const to = loc.end_line ?? loc.line;
  const lineNo = Math.min(Math.max(1, from), v.state.doc.lines);
  const line = v.state.doc.line(lineNo);
  const pos = Math.min(line.from + Math.max(0, (loc.col ?? 1) - 1), line.to);
  v.dispatch({
    selection: EditorSelection.cursor(pos),
    effects: [setHighlight.of({ from, to }), EditorView.scrollIntoView(pos, { y: "center" })],
  });
}
