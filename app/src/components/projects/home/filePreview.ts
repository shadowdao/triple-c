/**
 * What the Files viewer can show, and how much of it to ask for.
 *
 * Pure helpers, deliberately separate from the modal: the type sniffing is
 * where a preview quietly turns into a screenful of mojibake, and it is worth
 * testing without a container.
 */

/**
 * Extension → MIME, for the raster/vector types an `<img>` actually renders.
 * The MIME matters because the bytes are handed to the DOM as a `Blob`, and a
 * blob with the wrong (or empty) type will not decode.
 */
const IMAGE_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
  avif: "image/avif",
  // Safe in an `<img>`: that context cannot run the script an SVG may carry.
  svg: "image/svg+xml",
};

/** Extensions we are confident are text, so no byte sniffing is needed. */
const TEXT_EXTENSIONS = new Set([
  "txt", "md", "markdown", "rst", "log", "csv", "tsv",
  "json", "jsonc", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "properties",
  "js", "jsx", "mjs", "cjs", "ts", "tsx", "rs", "py", "rb", "go", "java", "kt",
  "c", "h", "cc", "cpp", "hpp", "cs", "php", "swift", "scala", "lua", "pl", "r",
  "sh", "bash", "zsh", "fish", "ps1", "bat",
  "html", "htm", "xml", "svelte", "vue", "css", "scss", "sass", "less",
  "sql", "graphql", "gql", "proto", "diff", "patch", "lock", "gitignore",
  "dockerfile", "makefile", "cmake", "gradle", "tf", "tfvars",
]);

/** Extensionless files that are text by convention. */
const TEXT_BASENAMES = new Set([
  "dockerfile", "makefile", "readme", "license", "licence", "changelog",
  "authors", "notice", "copying", "procfile", "rakefile", "gemfile", "vagrantfile",
  // Dotfiles: the leading dot is stripped before the lookup.
  "gitignore", "gitattributes", "gitmodules", "dockerignore", "npmrc", "nvmrc",
  "editorconfig", "bashrc", "zshrc", "profile", "env",
]);

/** 1 MiB of text is already far more than anyone reads in a modal. */
export const TEXT_PREVIEW_LIMIT = 1024 * 1024;
/**
 * Images get five times the budget: they are the file kind that routinely
 * blows past a text-sized cap, and a half-read image is not a preview at all —
 * it either decodes whole or it does not.
 */
export const IMAGE_PREVIEW_LIMIT = 5 * 1024 * 1024;

/** Lowercased extension, or "" for an extensionless name. */
export function extensionOf(name: string): string {
  const base = name.slice(name.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  // A leading dot is "hidden file", not "extension" (`.gitignore`).
  if (dot <= 0) return "";
  return base.slice(dot + 1).toLowerCase();
}

/** The MIME to build the Blob with, or null if this is not a previewable image. */
export function imageMimeFor(name: string): string | null {
  return IMAGE_MIME[extensionOf(name)] ?? null;
}

export type PreviewKind = "image" | "text" | "unknown";

/**
 * A first guess from the name alone. `unknown` is not a refusal — the viewer
 * reads the bytes and falls back to sniffing them, so a `.bak` of a config
 * file still previews.
 */
export function previewKind(name: string): PreviewKind {
  if (imageMimeFor(name)) return "image";
  const ext = extensionOf(name);
  if (ext) return TEXT_EXTENSIONS.has(ext) ? "text" : "unknown";
  const base = name.slice(name.lastIndexOf("/") + 1).replace(/^\./, "").toLowerCase();
  return TEXT_BASENAMES.has(base) ? "text" : "unknown";
}

/** How many bytes to ask the backend for, given what we expect to render. */
export function previewLimit(name: string): number {
  return previewKind(name) === "image" ? IMAGE_PREVIEW_LIMIT : TEXT_PREVIEW_LIMIT;
}

/** Base64 → bytes. `atob` yields a binary string; widen it one char at a time. */
export function decodeBase64(base64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/**
 * The classic heuristic: a NUL byte early on means this is not text. Cheap,
 * and it is what `git` and `grep` use to decide the same question.
 */
export function looksBinary(bytes: Uint8Array): boolean {
  const limit = Math.min(bytes.length, 8000);
  for (let i = 0; i < limit; i++) if (bytes[i] === 0) return true;
  return false;
}
