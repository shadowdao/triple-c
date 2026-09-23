/**
 * Extension → CodeMirror language, loaded on demand so a window only pays for
 * the grammar it shows. Dynamic `import()` becomes a same-origin chunk, fine
 * under `script-src 'self'`.
 */
import type { Extension } from "@codemirror/state";
import { extensionOf } from "../components/projects/home/filePreview";

type Loader = () => Promise<Extension>;

const BY_EXTENSION: Record<string, Loader> = {
  md: () => import("@codemirror/lang-markdown").then((m) => m.markdown()),
  markdown: () => import("@codemirror/lang-markdown").then((m) => m.markdown()),
  js: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  mjs: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  cjs: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  jsx: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ jsx: true })),
  ts: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ typescript: true })),
  tsx: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ jsx: true, typescript: true })),
  rs: () => import("@codemirror/lang-rust").then((m) => m.rust()),
  py: () => import("@codemirror/lang-python").then((m) => m.python()),
  json: () => import("@codemirror/lang-json").then((m) => m.json()),
  jsonc: () => import("@codemirror/lang-json").then((m) => m.json()),
  yaml: () => import("@codemirror/lang-yaml").then((m) => m.yaml()),
  yml: () => import("@codemirror/lang-yaml").then((m) => m.yaml()),
  css: () => import("@codemirror/lang-css").then((m) => m.css()),
  html: () => import("@codemirror/lang-html").then((m) => m.html()),
  htm: () => import("@codemirror/lang-html").then((m) => m.html()),
  toml: () => stream("toml"),
  lock: () => stream("toml"),
  sh: () => stream("shell"),
  bash: () => stream("shell"),
  zsh: () => stream("shell"),
};

const BY_BASENAME: Record<string, Loader> = {
  dockerfile: () => stream("shell"),
  makefile: () => stream("shell"),
};

async function stream(mode: "toml" | "shell"): Promise<Extension> {
  const { StreamLanguage } = await import("@codemirror/language");
  const parser = mode === "toml"
    ? (await import("@codemirror/legacy-modes/mode/toml")).toml
    : (await import("@codemirror/legacy-modes/mode/shell")).shell;
  return StreamLanguage.define(parser);
}

export function languageFor(path: string): Promise<Extension | null> {
  const ext = extensionOf(path);
  const base = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
  const loader = BY_EXTENSION[ext] ?? BY_BASENAME[base];
  return loader ? loader() : Promise.resolve(null);
}

const PROSE = new Set(["md", "markdown", "txt", "rst", "log", ""]);
export function wrapsLines(path: string): boolean {
  return PROSE.has(extensionOf(path));
}
