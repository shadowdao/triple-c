import { beforeAll, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { createRef } from "react";
import { EditorView } from "@codemirror/view";
import { CodeEditor, type CodeEditorHandle } from "./CodeEditor";

beforeAll(() => {
  // P17: CodeMirror's measure pass calls Range geometry, which jsdom lacks.
  Range.prototype.getClientRects = () => ({ length: 0, item: () => null, [Symbol.iterator]: [][Symbol.iterator] }) as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => ({ x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0, toJSON() {} }) as DOMRect;
});

const mount = (initialDoc: string) => {
  const ref = createRef<CodeEditorHandle>();
  const onDocChanged = vi.fn();
  const utils = render(
    <CodeEditor
      ref={ref}
      initialDoc={initialDoc}
      readOnly={false}
      language={null}
      lineWrapping={false}
      initialLocation={{ line: null, col: null, end_line: null }}
      onDocChanged={onDocChanged}
      onSave={() => {}}
    />,
  );
  const view = EditorView.findFromDOM(utils.container.querySelector(".cm-editor") as HTMLElement)!;
  return { ref, view, onDocChanged };
};

describe("CodeEditor.setDoc (a reload)", () => {
  it("keeps the cursor and the scroll position, and is not an edit", () => {
    const { ref, view, onDocChanged } = mount("one\ntwo\nthree\nfour\n");
    act(() => { view.dispatch({ selection: { anchor: 9 } }); }); // inside "three"
    // jsdom has no layout, so give the scroller a real, settable scrollTop.
    let top = 0;
    Object.defineProperty(view.scrollDOM, "scrollTop", { configurable: true, get: () => top, set: (v: number) => { top = v; } });
    view.scrollDOM.scrollTop = 120;

    act(() => { ref.current!.setDoc("one\ntwo\nTHREE\nfour\nfive\n"); });

    expect(view.state.doc.toString()).toBe("one\ntwo\nTHREE\nfour\nfive\n");
    expect(view.state.selection.main.head).toBe(9);
    expect(view.scrollDOM.scrollTop).toBe(120);
    expect(onDocChanged).not.toHaveBeenCalled();
  });

  it("clamps the cursor when the new text is shorter", () => {
    const { ref, view } = mount("a long first line\n");
    act(() => { view.dispatch({ selection: { anchor: 15 } }); });
    act(() => { ref.current!.setDoc("short"); });
    expect(view.state.selection.main.head).toBe(5);
  });

  it("a user edit is reported as a change", () => {
    const { view, onDocChanged } = mount("x");
    act(() => { view.dispatch({ changes: { from: 1, insert: "y" } }); });
    expect(onDocChanged).toHaveBeenCalledTimes(1);
  });
});
