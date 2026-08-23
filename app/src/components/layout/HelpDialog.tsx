import { useEffect, useRef, useCallback, useState } from "react";
import { getHelpContent } from "../../lib/tauri-commands";
import Modal from "../ui/Modal";
import Button from "../ui/Button";

interface Props {
  onClose: () => void;
}

/** Convert header text to a URL-friendly slug for anchor links. */
function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/<[^>]+>/g, "")       // strip HTML tags (e.g. from inline code)
    // Quote characters are escaped to entities before this runs (see
    // `renderMarkdown`). Drop those two entities whole, so a header with an
    // apostrophe or a quote slugifies to what it did when the character was
    // simply stripped — otherwise every such anchor id silently changes and
    // the in-document links pointing at it stop resolving. `&amp;`/`&lt;`/
    // `&gt;` are deliberately not in this list: they were already entities
    // before, so their existing (odd) slugs are the established ones.
    .replace(/&quot;|&#39;/g, "")
    .replace(/[^\w\s-]/g, "")      // remove non-word chars except spaces/dashes
    .replace(/\s+/g, "-")          // spaces to dashes
    .replace(/-+/g, "-")           // collapse consecutive dashes
    .replace(/^-|-$/g, "");        // trim leading/trailing dashes
}

/**
 * Escape a captured markdown value that is about to be interpolated into an
 * HTML *attribute* value.
 *
 * `renderMarkdown` entity-escapes the whole document first, but that pass only
 * covered `&`, `<` and `>` — not the quote characters, which is all an
 * attribute value is delimited by. `[x](https://a" onload="…)` therefore closed
 * `href="` and started a new attribute, because the URL capture is `[^)]+` and
 * `"` is in `[^)]`. The document is remote GitHub markdown, so that capture is
 * not ours to trust.
 *
 * Only quotes are escaped here: `&`, `<` and `>` have already been converted by
 * the caller, and re-escaping the `&` would double-encode every `&amp;` in a
 * query string.
 */
function attr(value: string): string {
  return value.replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

/**
 * Simple markdown-to-HTML converter for the help content.
 *
 * Exported for `HelpDialog.test.tsx`: the output goes to
 * `dangerouslySetInnerHTML`, so the escaping rules below are security rules and
 * need to be asserted rather than assumed.
 */
export function renderMarkdown(md: string): string {
  let html = md;

  // Normalize line endings
  html = html.replace(/\r\n/g, "\n");

  // Escape HTML entities (but we'll re-introduce tags below).
  //
  // The quote characters are part of this on purpose. Everything below builds
  // HTML by regex substitution, and several of those substitutions drop a
  // capture straight into an attribute value (`href="$2"`). Leaving `"` and `'`
  // live meant a link target could close the attribute and open another one —
  // in a document fetched from GitHub at runtime and handed to
  // `dangerouslySetInnerHTML`. Escaping here closes every such sink at the
  // source; `attr()` below is the belt to this pair of braces.
  html = html
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");

  // Fenced code blocks (```...```)
  html = html.replace(/```(\w*)\n([\s\S]*?)```/g, (_m, _lang, code) => {
    return `<pre class="help-code-block"><code>${code.trimEnd()}</code></pre>`;
  });

  // Inline code (`...`)
  html = html.replace(/`([^`]+)`/g, '<code class="help-inline-code">$1</code>');

  // Tables
  html = html.replace(
    /(?:^|\n)(\|.+\|)\n(\|[\s:|-]+\|)\n((?:\|.+\|\n?)+)/g,
    (_m, headerRow: string, _sep: string, bodyRows: string) => {
      const headers = headerRow
        .split("|")
        .slice(1, -1)
        .map((c: string) => `<th>${c.trim()}</th>`)
        .join("");
      const rows = bodyRows
        .trim()
        .split("\n")
        .map((row: string) => {
          const cells = row
            .split("|")
            .slice(1, -1)
            .map((c: string) => `<td>${c.trim()}</td>`)
            .join("");
          return `<tr>${cells}</tr>`;
        })
        .join("");
      return `<table class="help-table"><thead><tr>${headers}</tr></thead><tbody>${rows}</tbody></table>`;
    },
  );

  // Blockquotes (> ...)
  html = html.replace(/(?:^|\n)&gt; (.+)/g, '<blockquote class="help-blockquote">$1</blockquote>');
  // Merge adjacent blockquotes
  html = html.replace(/<\/blockquote>\s*<blockquote class="help-blockquote">/g, "<br/>");

  // Horizontal rules
  html = html.replace(/\n---\n/g, '<hr class="help-hr"/>');

  // Headers with id attributes for anchor navigation (process from h4 down to h1)
  html = html.replace(/^#### (.+)$/gm, (_m, title) => `<h4 class="help-h4" id="${slugify(title)}">${title}</h4>`);
  html = html.replace(/^### (.+)$/gm, (_m, title) => `<h3 class="help-h3" id="${slugify(title)}">${title}</h3>`);
  html = html.replace(/^## (.+)$/gm, (_m, title) => `<h2 class="help-h2" id="${slugify(title)}">${title}</h2>`);
  html = html.replace(/^# (.+)$/gm, (_m, title) => `<h1 class="help-h1" id="${slugify(title)}">${title}</h1>`);

  // Bold (**...**)
  html = html.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");

  // Italic (*...*)
  html = html.replace(/\*([^*]+)\*/g, "<em>$1</em>");

  // Markdown-style anchor links [text](#anchor)
  html = html.replace(
    /\[([^\]]+)\]\(#([^)]+)\)/g,
    (_m, text: string, anchor: string) =>
      `<a class="help-link" href="#${attr(anchor)}">${text}</a>`,
  );

  // Markdown-style external links [text](url)
  html = html.replace(
    /\[([^\]]+)\]\((https?:\/\/[^)]+)\)/g,
    (_m, text: string, url: string) =>
      `<a class="help-link" href="${attr(url)}" target="_blank" rel="noopener noreferrer">${text}</a>`,
  );

  // Unordered list items (- ...)
  // Group consecutive list items
  html = html.replace(/((?:^|\n)- .+(?:\n- .+)*)/g, (block) => {
    const items = block
      .trim()
      .split("\n")
      .map((line) => `<li>${line.replace(/^- /, "")}</li>`)
      .join("");
    return `<ul class="help-ul">${items}</ul>`;
  });

  // Ordered list items (1. ...)
  html = html.replace(/((?:^|\n)\d+\. .+(?:\n\d+\. .+)*)/g, (block) => {
    const items = block
      .trim()
      .split("\n")
      .map((line) => `<li>${line.replace(/^\d+\. /, "")}</li>`)
      .join("");
    return `<ol class="help-ol">${items}</ol>`;
  });

  // Links - convert bare URLs to clickable links (skip already-wrapped URLs)
  html = html.replace(
    /(?<!="|'>)(https?:\/\/[^\s<)]+)/g,
    (_m, url: string) =>
      `<a class="help-link" href="${attr(url)}" target="_blank" rel="noopener noreferrer">${url}</a>`,
  );

  // Wrap remaining loose text lines in paragraphs
  // Split by double newlines for paragraph breaks
  const blocks = html.split(/\n\n+/);
  html = blocks
    .map((block) => {
      const trimmed = block.trim();
      if (!trimmed) return "";
      // Don't wrap blocks that are already HTML elements
      if (
        /^<(h[1-4]|ul|ol|pre|table|blockquote|hr)/.test(trimmed)
      ) {
        return trimmed;
      }
      // Wrap in paragraph, replacing single newlines with <br/>
      return `<p class="help-p">${trimmed.replace(/\n/g, "<br/>")}</p>`;
    })
    .join("\n");

  return html;
}

export default function HelpDialog({ onClose }: Props) {
  const contentRef = useRef<HTMLDivElement>(null);
  const [markdown, setMarkdown] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getHelpContent()
      .then(setMarkdown)
      .catch((e) => setError(String(e)));
  }, []);

  // Handle anchor link clicks to scroll within the dialog
  const handleContentClick = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    const anchor = target.closest("a");
    if (!anchor) return;
    const href = anchor.getAttribute("href");
    if (!href || !href.startsWith("#")) return;
    e.preventDefault();
    const el = contentRef.current?.querySelector(href);
    if (el) el.scrollIntoView({ behavior: "smooth" });
  }, []);

  return (
    <Modal
      title="How to Use Triple-C"
      onClose={onClose}
      widthClassName="w-[48rem]"
      footer={<Button onClick={onClose}>Close</Button>}
    >
      <div ref={contentRef} onClick={handleContentClick} className="help-content">
        {error && (
          <p className="text-[var(--error)] text-sm">
            Failed to load help content: {error}
          </p>
        )}
        {!markdown && !error && (
          <p className="text-[var(--text-secondary)] text-sm">Loading…</p>
        )}
        {markdown && (
          <div dangerouslySetInnerHTML={{ __html: renderMarkdown(markdown) }} />
        )}
      </div>
    </Modal>
  );
}
