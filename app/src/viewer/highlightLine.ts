import { StateEffect, StateField, type Extension, type Text } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";

export const setHighlight = StateEffect.define<{ from: number; to: number } | null>();

const lineMark = Decoration.line({ class: "cm-triple-c-target" });

export function lineRangeToPositions(doc: Text, from: number, to: number): { from: number; to: number } | null {
  const clamp = (n: number) => Math.min(Math.max(1, Math.floor(n)), doc.lines);
  const a = clamp(from);
  const b = Math.max(a, clamp(to));
  return { from: doc.line(a).from, to: doc.line(b).from };
}

export const highlightLineField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, tr) {
    let next = value.map(tr.changes);
    for (const e of tr.effects) {
      if (!e.is(setHighlight)) continue;
      if (e.value === null) { next = Decoration.none; continue; }
      const range = lineRangeToPositions(tr.state.doc, e.value.from, e.value.to);
      if (!range) { next = Decoration.none; continue; }
      const marks = [];
      for (let pos = range.from; pos <= range.to; ) {
        const line = tr.state.doc.lineAt(pos);
        marks.push(lineMark.range(line.from));
        if (line.to + 1 > tr.state.doc.length) break;
        pos = line.to + 1;
      }
      next = Decoration.set(marks, true);
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export function highlightExtension(): Extension {
  return [highlightLineField];
}
