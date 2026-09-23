/**
 * xterm `ILinkProvider` for file paths in the buffer.
 *
 * Registered after `WebLinksAddon` so URLs are claimed first; `findFilePathLinks`
 * also refuses anything inside a `scheme://` span, so the two never overlap.
 * Ranges are 1-based on both axes with an *inclusive* end column (xterm's
 * contract), and `provideLinks`' row is 1-based while `getLine` is 0-based.
 */
import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { findFilePathLinks, type FilePathMatch } from "../../lib/filePathLinks";
import { joinWrappedRows, offsetToCell } from "../../lib/xtermLineJoin";

export interface FilePathHover {
  /** `path` is the raw matched path — relative paths stay relative. */
  show(path: string): void;
  hide(): void;
}

export function createFilePathLinkProvider(
  term: Pick<Terminal, "buffer">,
  onOpen: (match: FilePathMatch) => void,
  gate: (event: MouseEvent) => boolean,
  hover?: FilePathHover,
): ILinkProvider {
  return {
    provideLinks(bufferLineNumber, callback) {
      const row = bufferLineNumber - 1;
      const line = term.buffer.active.getLine(row);
      if (!line) return callback(undefined);
      const joined = joinWrappedRows(term.buffer.active, row);
      // `offsetToCell` has no row to map onto when nothing was joined.
      if (joined.rowStarts.length === 0) return callback(undefined);
      const matches = findFilePathLinks(joined.text);
      if (matches.length === 0) return callback(undefined);
      const links: ILink[] = matches
        .map((m): ILink => ({
          range: { start: offsetToCell(joined, m.start), end: offsetToCell(joined, m.end - 1) },
          text: joined.text.slice(m.start, m.end),
          decorations: { pointerCursor: true, underline: true },
          activate: (event) => {
            if (!gate(event)) return;
            onOpen(m);
          },
          hover: () => hover?.show(m.path),
          leave: () => hover?.hide(),
        }))
        // Only links that touch the row being asked about (xterm asks per row).
        .filter((l) => l.range.start.y <= bufferLineNumber && l.range.end.y >= bufferLineNumber);
      callback(links.length ? links : undefined);
    },
  };
}
