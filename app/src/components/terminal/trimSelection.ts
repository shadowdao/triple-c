/**
 * Cleans up terminal selections for pasting into other tools.
 *
 * Terminal UI padding (left margin from the xterm container, alignment spaces
 * at end of line) ends up in the copied text. This helper removes that cruft
 * while preserving the *relative* indentation of the content — so code blocks
 * keep their shape but lose the wrapper padding.
 *
 * Steps:
 *   1. Dedent — strip the common leading whitespace count from every line.
 *   2. trimEnd — drop trailing whitespace per line.
 *   3. Drop fully-blank leading and trailing lines.
 *
 * Internal newlines and relative indentation are preserved. Pure function.
 */
export function trimSelection(text: string): string {
  if (!text) return text;

  const lines = text.split("\n");

  let minIndent = Infinity;
  for (const line of lines) {
    if (line.trim() === "") continue;
    const match = line.match(/^[ \t]*/);
    const indent = match ? match[0].length : 0;
    if (indent < minIndent) minIndent = indent;
    if (minIndent === 0) break;
  }
  if (!Number.isFinite(minIndent)) minIndent = 0;

  const processed = lines.map((line) => {
    const afterDedent = line.length >= minIndent ? line.slice(minIndent) : "";
    return afterDedent.trimEnd();
  });

  let start = 0;
  let end = processed.length;
  while (start < end && processed[start] === "") start++;
  while (end > start && processed[end - 1] === "") end--;

  return processed.slice(start, end).join("\n");
}
