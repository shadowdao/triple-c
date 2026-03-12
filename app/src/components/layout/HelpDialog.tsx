import { useEffect, useRef, useCallback, useState } from "react";
import { getHelpContent } from "../../lib/tauri-commands";

interface Props {
  onClose: () => void;
}

/** Convert header text to a URL-friendly slug for anchor links. */
function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/<[^>]+>/g, "")       // strip HTML tags (e.g. from inline code)
    .replace(/[^\w\s-]/g, "")      // remove non-word chars except spaces/dashes
    .replace(/\s+/g, "-")          // spaces to dashes
    .replace(/-+/g, "-")           // collapse consecutive dashes
    .replace(/^-|-$/g, "");        // trim leading/trailing dashes
}

/** Simple markdown-to-HTML converter for the help content. */
function renderMarkdown(md: string): string {
  let html = md;

  // Normalize line endings
  html = html.replace(/\r\n/g, "\n");

  // Escape HTML entities (but we'll re-introduce tags below)
  html = html.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

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
    '<a class="help-link" href="#$2">$1</a>',
  );

  // Markdown-style external links [text](url)
  html = html.replace(
    /\[([^\]]+)\]\((https?:\/\/[^)]+)\)/g,
    '<a class="help-link" href="$2" target="_blank" rel="noopener noreferrer">$1</a>',
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
    '<a class="help-link" href="$1" target="_blank" rel="noopener noreferrer">$1</a>',
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
  const overlayRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [markdown, setMarkdown] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  useEffect(() => {
    getHelpContent()
      .then(setMarkdown)
      .catch((e) => setError(String(e)));
  }, []);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onClose();
    },
    [onClose],
  );

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
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg shadow-xl w-[48rem] max-w-[90vw] max-h-[85vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-[var(--border-color)] flex-shrink-0">
          <h2 className="text-lg font-semibold">How to Use Triple-C</h2>
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
          >
            Close
          </button>
        </div>

        {/* Scrollable content */}
        <div
          ref={contentRef}
          onClick={handleContentClick}
          className="flex-1 overflow-y-auto px-6 py-4 help-content"
        >
          {error && (
            <p className="text-[var(--error)] text-sm">Failed to load help content: {error}</p>
          )}
          {!markdown && !error && (
            <p className="text-[var(--text-secondary)] text-sm">Loading...</p>
          )}
          {markdown && (
            <div dangerouslySetInnerHTML={{ __html: renderMarkdown(markdown) }} />
          )}
        </div>
      </div>
    </div>
  );
}
