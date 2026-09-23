import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import type { Extension } from "@codemirror/state";

// Syntax colours come from the `--syntax-*` tokens in index.css (P12), not
// hard-coded hex, even though the values match the GitHub-dark ANSI palette
// TerminalView.tsx already uses.
export const viewerTheme: Extension = [
  EditorView.theme(
    {
      "&": { backgroundColor: "var(--bg-primary)", color: "var(--text-primary)", height: "100%", fontSize: "13px" },
      ".cm-content": { fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', Menlo, Monaco, monospace", caretColor: "var(--accent)" },
      ".cm-scroller": { overflow: "auto" },
      ".cm-gutters": { backgroundColor: "var(--bg-secondary)", color: "var(--text-secondary)", borderRight: "1px solid var(--border-color)" },
      ".cm-activeLine": { backgroundColor: "var(--accent-muted)" },
      ".cm-activeLineGutter": { backgroundColor: "var(--accent-muted)" },
      ".cm-triple-c-target": { backgroundColor: "var(--warning-muted)", outline: "1px solid var(--warning)" },
      "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": { backgroundColor: "var(--accent-muted)" },
      ".cm-panels": { backgroundColor: "var(--bg-secondary)", color: "var(--text-primary)", borderBottom: "1px solid var(--border-color)" },
      ".cm-searchMatch": { backgroundColor: "var(--warning-muted)", outline: "1px solid var(--warning)" },
      ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--success-muted)" },
    },
    { dark: true },
  ),
  syntaxHighlighting(
    HighlightStyle.define([
      { tag: [t.keyword, t.modifier, t.operatorKeyword], color: "var(--syntax-keyword)" },
      { tag: [t.string, t.special(t.string)], color: "var(--syntax-string)" },
      { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--text-secondary)", fontStyle: "italic" },
      { tag: [t.number, t.bool, t.null, t.atom], color: "var(--syntax-number)" },
      { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "var(--syntax-function)" },
      { tag: [t.typeName, t.className, t.namespace], color: "var(--syntax-type)" },
      { tag: [t.propertyName, t.attributeName], color: "var(--syntax-property)" },
      { tag: t.heading, fontWeight: "bold", color: "var(--accent)" },
      { tag: t.emphasis, fontStyle: "italic" },
      { tag: t.strong, fontWeight: "bold" },
      { tag: t.link, color: "var(--accent)", textDecoration: "underline" },
      { tag: t.invalid, color: "var(--syntax-keyword)", textDecoration: "underline wavy" },
    ]),
  ),
];
