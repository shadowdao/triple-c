import { describe, it, expect } from "vitest";
import { existsSync, readdirSync, readFileSync, statSync } from "fs";
import { dirname, join, relative, resolve, sep } from "path";
import { builtinModules } from "module";
import ts from "typescript";

/**
 * The capability files are the IPC ACL. Since the AppManifest lockdown, a window can only
 * invoke the app commands its file grants; `build.rs` proves every command is granted in the
 * file its name says it belongs to. This proves the other half: the code that *runs* in each
 * window imports only wrappers that window is granted. A wrapper imported on the wrong side
 * fails here, not with `Command … not allowed by ACL` in a release build.
 *
 * It works on imports rather than `invoke(` literals because the viewer never calls invoke:
 * everything goes through `lib/tauri-commands.ts`, which is the only file allowed to import
 * `@tauri-apps/api/core` (that rule is what makes this test complete).
 *
 * Every file is parsed with the TypeScript compiler (`ts.createSourceFile`), not scanned with
 * regexes, so comments, strings, template substitutions and regex literals are the parser's
 * problem rather than ours. Anything the walk below cannot account for — a computed `import()`,
 * a path alias, a namespace of the wrappers handed around as a value — throws (fail-closed);
 * nothing is ever skipped quietly.
 */
const srcDir = resolve(__dirname, "..");
const capDir = resolve(srcDir, "../src-tauri/capabilities");
const nodeModulesDir = resolve(srcDir, "../node_modules");
const WRAPPERS = resolve(srcDir, "lib/tauri-commands.ts");
const VIEWER_ENTRY = resolve(srcDir, "viewer/main.tsx");

const toPermission = (command: string) => `allow-${command.replace(/_/g, "-")}`;

function readCapability(file: string) {
  const cap = JSON.parse(readFileSync(resolve(capDir, file), "utf-8")) as {
    windows: string[];
    permissions: (string | { identifier: string })[];
  };
  const ids = cap.permissions.map((p) => (typeof p === "string" ? p : p.identifier));
  return {
    windows: cap.windows,
    bare: ids.filter((id) => !id.includes(":")).sort(),
    prefixed: ids.filter((id) => id.includes(":")).sort(),
  };
}

/** A code extension: anything Vite would run as a module rather than serve as an asset. */
const CODE_EXTENSION = /\.(mjs|js|mts|ts|jsx|tsx|cjs|cts)$/;

/** Every code file under src/, tests and src/test included. */
function codeFiles(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) codeFiles(path, out);
    else if (CODE_EXTENSION.test(name)) out.push(path);
  }
  return out;
}

/** Code that ships in a window: not under src/test, not a `*.test.*`, not a declaration file. */
const isAppSource = (file: string) =>
  !relative(srcDir, file).startsWith(`test${sep}`) && !/\.test\.[^./]+$/.test(file) && !/\.d\.[cm]?ts$/.test(file);

const rel = (file: string) => relative(srcDir, file);

function fail(file: string, node: ts.Node | undefined, message: string): never {
  const where = node
    ? `:${node.getSourceFile().getLineAndCharacterOfPosition(node.getStart()).line + 1}`
    : "";
  throw new Error(`${rel(file)}${where}: ${message}`);
}

function parse(file: string): ts.SourceFile {
  const kind = /\.[jt]sx$/.test(file) ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
  return ts.createSourceFile(file, readFileSync(file, "utf-8"), ts.ScriptTarget.Latest, true, kind);
}

/** Depth-first visit of every node (JSDoc is not a child, so comments never show up). */
function walk(node: ts.Node, visit: (n: ts.Node) => void) {
  visit(node);
  ts.forEachChild(node, (child) => walk(child, visit));
}

/** Specifiers that reach Tauri's raw `invoke`: `core` itself, and the package root, which
 * re-exports it as `core`. Only `lib/tauri-commands.ts` may use either. */
const INVOKE_SPECIFIER = /^@tauri-apps\/api(\/(core|index)(\.[cm]?js)?)?\/?$/;

/** Vite 6's default `resolve.extensions`, in its order (vite.config.ts does not override it). */
const VITE_EXTENSIONS = [".mjs", ".js", ".mts", ".ts", ".jsx", ".tsx", ".json"];
/** A bare npm package name (optionally scoped) followed by an optional subpath. */
const PACKAGE_NAME = /^((?:@[a-z0-9][\w.-]*\/)?[a-z0-9][\w.-]*)(\/.*)?$/i;

const isFile = (p: string) => existsSync(p) && statSync(p).isFile();
const isDir = (p: string) => existsSync(p) && statSync(p).isDirectory();

/**
 * Vite 6's `tryCleanFsResolve` for a relative path, step for step, so the file analysed is the
 * file Vite would load: the exact path if it is a file; else a `.js`/`.mjs`/`.cjs`/`.jsx` path's
 * TypeScript twin (`.js` → `.ts`, then `.tsx`); else `path + ext` over VITE_EXTENSIONS in order
 * (so `shadow.mjs` beats `shadow.ts`, and `./evil.impl` finds `evil.impl.ts`); else, for a
 * directory, `index + ext` in the same order. A directory with a package.json would switch Vite to
 * package-entry resolution, which this test does not model, so it fails closed.
 */
function viteResolveRelative(path: string, from: string, node: ts.Node): string | undefined {
  if (isFile(path)) return path;
  if (/\.(?:js|mjs|cjs|jsx)$/.test(path)) {
    const ext = path.slice(path.lastIndexOf("."));
    const stem = path.slice(0, -ext.length);
    const twin = [stem + ext.replace("js", "ts"), ...(ext === ".js" ? [`${stem}.tsx`] : [])].find(isFile);
    if (twin) return twin;
  }
  const withExt = VITE_EXTENSIONS.map((e) => path + e).find(isFile);
  if (withExt) return withExt;
  if (isDir(path)) {
    if (existsSync(join(path, "package.json"))) {
      fail(from, node, `imports directory ${rel(path)}, which has a package.json this test does not model`);
    }
    return VITE_EXTENSIONS.map((e) => join(path, `index${e}`)).find(isFile);
  }
  return undefined;
}

/**
 * Resolves a module specifier to the source file it names, or `null` for something that is not
 * part of `src/`'s module graph (a real package, a node builtin, an asset). Everything else
 * throws: a path alias, a Vite query suffix (`?worker`, `?raw`), a relative path that leaves
 * `src/` or names nothing.
 */
function resolveSpecifier(from: string, spec: string, node: ts.Node): string | null {
  if (spec.includes("?") || spec.includes("#")) {
    fail(from, node, `import "${spec}" carries a query/fragment suffix this test cannot audit`);
  }
  if (spec.startsWith("./") || spec.startsWith("../")) {
    const found = viteResolveRelative(resolve(dirname(from), spec), from, node);
    if (!found) fail(from, node, `cannot resolve import "${spec}"`);
    if (rel(found).startsWith("..")) fail(from, node, `import "${spec}" resolves outside src/ (${found})`);
    return CODE_EXTENSION.test(found) ? found : null; // css, svg, json, … — an asset, not a module
  }
  if (spec.startsWith("node:") || builtinModules.includes(spec)) return null;
  const pkg = PACKAGE_NAME.exec(spec)?.[1];
  if (pkg && existsSync(join(nodeModulesDir, pkg, "package.json"))) return null;
  fail(
    from,
    node,
    `import "${spec}" is neither relative nor an installed package — likely a path alias. This test ` +
      `only understands relative imports and real dependencies; teach it the alias rather than letting ` +
      `the file drop out of the closure.`,
  );
}

const stringLiteralText = (node: ts.Node | undefined) =>
  node && (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) ? node.text : undefined;

interface ModuleFacts {
  /** Every module specifier the file names: static imports, `export … from`, literal `import()`. */
  specifiers: string[];
  /** The source files those specifiers resolve to (packages and assets excluded). */
  targets: string[];
  /** Wrapper names the file reaches from `lib/tauri-commands.ts`. */
  wrapperNames: string[];
}

/**
 * Parses one file and returns its module edges and the wrappers it reaches. Wrapper usage is:
 * named imports and named re-exports (by their exported name), and `X.name` / `X?.name` (or
 * `typeof X.name` in a type) where `X` is a namespace import of the wrappers. Fails closed on
 * everything else that could carry a wrapper: a default import, `export *` / `export * as` of the wrappers, `import()` of them (it
 * resolves to the namespace object), a computed `import()`, `require`, `import X = require`,
 * `import.meta.glob`, and any reference to a namespace alias other than `X.name`.
 *
 * Namespace references are matched by identifier text, not symbol: every Identifier spelled `X`
 * anywhere in the file must be the object of a property access (or the name *of* one, `o.X`,
 * which is not a reference). A local that shadows `X` needs a declaration spelled `X`, and that
 * declaration is itself such an Identifier, so shadowing fails closed rather than confusing it.
 */
function analyzeModule(file: string): ModuleFacts {
  const sf = parse(file);
  const facts: ModuleFacts = { specifiers: [], targets: [], wrapperNames: [] };
  const namespaceAliases = new Map<string, ts.Identifier>();

  const edge = (spec: string, node: ts.Node) => {
    facts.specifiers.push(spec);
    const target = resolveSpecifier(file, spec, node);
    if (target) facts.targets.push(target);
    return target;
  };

  for (const stmt of sf.statements) {
    if (ts.isImportDeclaration(stmt)) {
      const target = edge(stringLiteralText(stmt.moduleSpecifier)!, stmt);
      const clause = stmt.importClause;
      if (target !== WRAPPERS || !clause) continue;
      if (clause.name) fail(file, stmt, "default-imports tauri-commands.ts, which has no default export");
      const bindings = clause.namedBindings;
      if (bindings && ts.isNamespaceImport(bindings)) namespaceAliases.set(bindings.name.text, bindings.name);
      if (bindings && ts.isNamedImports(bindings)) {
        for (const el of bindings.elements) facts.wrapperNames.push((el.propertyName ?? el.name).text);
      }
    } else if (ts.isExportDeclaration(stmt) && stmt.moduleSpecifier) {
      const target = edge(stringLiteralText(stmt.moduleSpecifier)!, stmt);
      if (target !== WRAPPERS) continue;
      const clause = stmt.exportClause;
      if (!clause || ts.isNamespaceExport(clause)) {
        fail(
          file,
          stmt,
          `re-exports tauri-commands.ts with "export *${clause ? " as …" : ""}", which cannot be audited — ` +
            `re-export wrappers by name (export { wrapperName } from "…/tauri-commands")`,
        );
      }
      for (const el of clause.elements) facts.wrapperNames.push((el.propertyName ?? el.name).text);
    } else if (ts.isImportEqualsDeclaration(stmt) && ts.isExternalModuleReference(stmt.moduleReference)) {
      fail(file, stmt, `"import … = require(…)" is not followed by this test; use an ES import`);
    }
  }

  walk(sf, (node) => {
    if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
      const spec = stringLiteralText(node.arguments[0]);
      if (spec === undefined || node.arguments.length === 0) {
        fail(file, node, "import() with a computed specifier cannot be followed; use a string literal");
      }
      if (edge(spec, node) === WRAPPERS) {
        fail(file, node, "import() of tauri-commands.ts yields the whole namespace object; import wrappers by name");
      }
    } else if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === "require") {
      fail(file, node, "require() is not followed by this test; use an ES import");
    } else if (
      ts.isPropertyAccessExpression(node) &&
      ts.isMetaProperty(node.expression) &&
      node.expression.keywordToken === ts.SyntaxKind.ImportKeyword &&
      node.name.text.startsWith("glob")
    ) {
      fail(file, node, "import.meta.glob pulls in modules this test cannot enumerate");
    } else if (ts.isIdentifier(node) && namespaceAliases.has(node.text)) {
      if (node === namespaceAliases.get(node.text)) return; // the `import * as X` binding itself
      const parent = node.parent;
      if (ts.isPropertyAccessExpression(parent) && parent.name === node) return; // `o.X` — not a reference
      if (ts.isPropertyAccessExpression(parent) && parent.expression === node && ts.isIdentifier(parent.name)) {
        facts.wrapperNames.push(parent.name.text);
        return;
      }
      // `typeof X.name` in a type: a QualifiedName, type-only, counted anyway (the safe direction).
      if (ts.isQualifiedName(parent) && parent.left === node && ts.isTypeQueryNode(parent.parent)) {
        facts.wrapperNames.push(parent.right.text);
        return;
      }
      fail(
        file,
        node,
        `"${node.text}" (a namespace import of tauri-commands.ts) is used in \`${parent.getText().slice(0, 60)}\` ` +
          `rather than as "${node.text}.wrapperName" — only direct member access can be audited; import ` +
          `the wrappers by name instead`,
      );
    }
  });
  return facts;
}

/**
 * `export const NAME = … invoke<T>("command", …)` → NAME → command, read from the AST: each exported
 * const calls `invoke` exactly once, with a string literal. `invoke` must be imported by name from
 * `@tauri-apps/api/core` and appear nowhere except as the callee of such a call.
 */
function wrapperCommands(): Map<string, string> {
  const sf = parse(WRAPPERS);
  const map = new Map<string, string>();
  const callsByWrapper = new Map<string, ts.CallExpression[]>();
  for (const stmt of sf.statements) {
    if (ts.isImportDeclaration(stmt) && INVOKE_SPECIFIER.test(stringLiteralText(stmt.moduleSpecifier)!)) {
      const b = stmt.importClause?.namedBindings;
      const onlyInvoke =
        !stmt.importClause?.name &&
        b !== undefined &&
        ts.isNamedImports(b) &&
        b.elements.every((el) => !el.propertyName && el.name.text === "invoke");
      if (!onlyInvoke) fail(WRAPPERS, stmt, `must import exactly { invoke } from ${stringLiteralText(stmt.moduleSpecifier)}`);
    }
    if (
      ts.isVariableStatement(stmt) &&
      stmt.modifiers?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword) &&
      stmt.declarationList.flags & ts.NodeFlags.Const
    ) {
      for (const decl of stmt.declarationList.declarations) {
        if (!ts.isIdentifier(decl.name)) fail(WRAPPERS, decl, "an exported wrapper must be a plain `export const NAME`");
        callsByWrapper.set(decl.name.text, []);
      }
    }
  }
  walk(sf, (node) => {
    if (
      ts.isCallExpression(node) &&
      (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
        (ts.isIdentifier(node.expression) && node.expression.text === "require"))
    ) {
      fail(WRAPPERS, node, "tauri-commands.ts may not load modules dynamically (import()/require())");
    }
    if (!ts.isIdentifier(node) || node.text !== "invoke" || ts.isImportSpecifier(node.parent)) return;
    const call = node.parent;
    if (!ts.isCallExpression(call) || call.expression !== node) {
      fail(WRAPPERS, node, "invoke is referenced other than as a direct call");
    }
    let decl: ts.Node = call;
    while (!(ts.isVariableDeclaration(decl) && decl.parent.parent.parent === sf)) {
      decl = decl.parent;
      if (decl === sf) fail(WRAPPERS, call, "invoke is called outside an `export const` wrapper");
    }
    const calls = callsByWrapper.get((decl as ts.VariableDeclaration).name.getText());
    if (!calls) fail(WRAPPERS, call, "invoke is called outside an `export const` wrapper");
    // Only inside the wrapper's own function body: anything else (`export const x = invoke(…)`, an
    // IIFE, a default argument) runs at module load in every window that imports this file.
    const init = (decl as ts.VariableDeclaration).initializer;
    const inBody =
      init !== undefined &&
      (ts.isArrowFunction(init) || ts.isFunctionExpression(init)) &&
      call.pos >= init.body.pos &&
      call.end <= init.body.end &&
      !enclosedInIife(call, init);
    if (!inBody) fail(WRAPPERS, call, "invoke must be called inside the wrapper's function body, not at module load");
    calls.push(call);
  });
  for (const [name, calls] of callsByWrapper) {
    expect(calls, `${name} must call invoke exactly once`).toHaveLength(1);
    const command = stringLiteralText(calls[0].arguments[0]) ?? "<not a string literal>";
    expect(command, `${name} must invoke a string literal (a computed name cannot be audited)`).toMatch(
      /^[a-z0-9_]+$/,
    );
    map.set(name, command);
  }
  expect(map.size).toBeGreaterThan(100);
  return map;
}

/** Whether `node` sits in a function expression that is called on the spot, between it and `outer`. */
function enclosedInIife(node: ts.Node, outer: ts.Node): boolean {
  for (let n = node.parent; n !== outer; n = n.parent) {
    let fn: ts.Node = n;
    if (!(ts.isArrowFunction(fn) || ts.isFunctionExpression(fn))) continue;
    while (ts.isParenthesizedExpression(fn.parent)) fn = fn.parent;
    if (ts.isCallExpression(fn.parent) && fn.parent.expression === fn) return true;
  }
  return false;
}

/** Module specifiers a file names — static imports, `export … from`, `import … = require`, and the
 * argument of `import()`/`require()` — without resolving anything or applying the closure rules,
 * so it can run over test files too. A computed `import()`/`require()` argument yields `null`. */
function namedSpecifiers(file: string): (string | null)[] {
  const out: (string | null)[] = [];
  walk(parse(file), (node) => {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier) {
      out.push(stringLiteralText(node.moduleSpecifier) ?? null);
    } else if (ts.isExternalModuleReference(node)) {
      out.push(stringLiteralText(node.expression) ?? null);
    } else if (
      ts.isCallExpression(node) &&
      (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
        (ts.isIdentifier(node.expression) && node.expression.text === "require"))
    ) {
      out.push(stringLiteralText(node.arguments[0]) ?? null);
    }
  });
  return out;
}

const factsCache = new Map<string, ModuleFacts>();
function factsOf(file: string): ModuleFacts {
  let facts = factsCache.get(file);
  if (!facts) {
    facts = analyzeModule(file);
    factsCache.set(file, facts);
  }
  return facts;
}

/** Transitive closure from the viewer entry over static imports, `export … from` and `import()`. */
function viewerClosure(): Set<string> {
  const seen = new Set<string>();
  const queue = [VIEWER_ENTRY];
  while (queue.length > 0) {
    const file = queue.pop()!;
    if (seen.has(file)) continue;
    seen.add(file);
    for (const target of factsOf(file).targets) if (!seen.has(target)) queue.push(target);
  }
  return seen;
}

describe("capability files match the code each window runs", () => {
  const defaultCap = readCapability("default.json");
  const viewerCap = readCapability("file-viewer.json");
  const allCode = codeFiles(srcDir);
  const files = allCode.filter(isAppSource);

  it("only lib/tauri-commands.ts imports @tauri-apps/api/core", () => {
    // Every code file, tests included: the viewer closure can reach anything a relative import can.
    const offenders = allCode
      .filter((f) => f !== WRAPPERS)
      .filter((f) => namedSpecifiers(f).some((s) => s === null || INVOKE_SPECIFIER.test(s)))
      .map(rel);
    expect(offenders, "imports @tauri-apps/api(/core), or loads a computed specifier").toEqual([]);
  });

  it("the windows lists are the reviewed ones", () => {
    expect(defaultCap.windows).toEqual(["main"]);
    expect(viewerCap.windows).toEqual(["file-viewer-*"]);
  });

  it("the plugin/core grants are the reviewed ones", () => {
    expect(defaultCap.prefixed).toEqual([
      "core:event:allow-listen",
      "core:event:allow-unlisten",
      "core:webview:allow-internal-toggle-devtools",
      "dialog:allow-open",
      "dialog:allow-save",
    ]);
    expect(viewerCap.prefixed).toEqual([
      "core:event:allow-listen",
      "core:event:allow-unlisten",
      "core:webview:allow-internal-toggle-devtools",
      "core:window:allow-destroy",
    ]);
  });

  it("the viewer window imports exactly the wrappers file-viewer.json grants", () => {
    const wrappers = wrapperCommands();
    const closure = viewerClosure();
    expect(closure.has(WRAPPERS), "the viewer reaches tauri-commands.ts").toBe(true);
    const viewerCommands = new Set<string>();
    for (const file of closure) {
      for (const name of factsOf(file).wrapperNames) {
        const command = wrappers.get(name);
        expect(command, `${rel(file)} imports unknown wrapper ${name}`).toBeDefined();
        viewerCommands.add(command!);
      }
    }
    const granted = [...viewerCommands].map(toPermission).sort();
    expect(granted).toEqual(viewerCap.bare);
  });

  it("the main window imports only wrappers default.json grants, and none of the viewer's", () => {
    const wrappers = wrapperCommands();
    const closure = viewerClosure();
    const mainCommands = new Set<string>();
    for (const file of files) {
      if (closure.has(file)) continue;
      for (const name of factsOf(file).wrapperNames) {
        const command = wrappers.get(name);
        expect(command, `${rel(file)} imports unknown wrapper ${name}`).toBeDefined();
        mainCommands.add(command!);
      }
    }
    expect(mainCommands.size).toBeGreaterThan(50);
    const ungranted = [...mainCommands].map(toPermission).filter((p) => !defaultCap.bare.includes(p)).sort();
    expect(ungranted, "main-window code imports wrappers default.json does not grant").toEqual([]);
    const crossed = [...mainCommands].filter((c) => viewerCap.bare.includes(toPermission(c))).sort();
    expect(crossed, "main-window code imports viewer-only wrappers").toEqual([]);
  });

  it("every wrapper's command is granted in exactly one capability file", () => {
    const wrappers = wrapperCommands();
    const both: string[] = [];
    const neither: string[] = [];
    for (const command of new Set(wrappers.values())) {
      const p = toPermission(command);
      const inDefault = defaultCap.bare.includes(p);
      const inViewer = viewerCap.bare.includes(p);
      if (inDefault && inViewer) both.push(command);
      if (!inDefault && !inViewer) neither.push(command);
    }
    expect(both).toEqual([]);
    expect(neither, "granted nowhere — cargo check would fail too, but you may not have run it").toEqual([]);
  });
});
