import { describe, it, expect } from "vitest";
import { renderMarkdown } from "./HelpDialog";

/**
 * `renderMarkdown` builds HTML by regex substitution and the result is handed
 * to `dangerouslySetInnerHTML`. Its input is the help document, which is
 * fetched from GitHub at runtime — remote, versioned by someone else, and not
 * something the app gets to trust. These tests pin the escaping.
 */
/**
 * Parse rendered HTML and return its first anchor, asserting that *no* element
 * anywhere in the output grew an attribute outside the allowed set. A broken
 * attribute value is only interesting if it becomes an attribute, so the check
 * has to run through a real parser rather than over the string.
 */
const ALLOWED_ATTRS = new Set(["class", "href", "target", "rel", "id"]);

function onlyAnchor(html: string): HTMLAnchorElement {
  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const el of Array.from(doc.body.querySelectorAll("*"))) {
    for (const name of attrNames(el)) {
      expect(ALLOWED_ATTRS.has(name), `unexpected attribute ${name}`).toBe(true);
    }
  }
  const anchors = doc.querySelectorAll("a");
  expect(anchors.length).toBeGreaterThan(0);
  return anchors[0] as HTMLAnchorElement;
}

/** Attribute names the parser actually saw on an element. */
function attrNames(el: Element): string[] {
  return Array.from(el.attributes).map((a) => a.name);
}

describe("renderMarkdown escaping", () => {
  it("escapes the quote characters an attribute value is delimited by", () => {
    const html = renderMarkdown('He said "hi" and it\'s fine.');
    expect(html).not.toMatch(/said "hi"/);
    expect(html).toContain("&quot;hi&quot;");
    expect(html).toContain("it&#39;s");
  });

  it("does not let a link target break out of href=\"…\"", () => {
    // The sink: the URL capture is `[^)]+`, which includes `"` and spaces, and
    // the value lands directly inside `href="…"`. Asserted through the DOM,
    // not by string matching — the payload text legitimately survives *inside*
    // the attribute value; what must not happen is it becoming an attribute.
    const a = onlyAnchor(
      renderMarkdown(
        '[click](https://example.com/" onmouseover="steal() formaction="https://evil.example)',
      ),
    );
    expect(attrNames(a)).toEqual(["class", "href", "target", "rel"]);
    expect(a.getAttribute("href")).toContain('" onmouseover="');
  });

  it("does not let an in-document anchor break out of href=\"#…\"", () => {
    const a = onlyAnchor(
      renderMarkdown('[jump](#top" onfocus="steal() autofocus="x)'),
    );
    expect(attrNames(a)).toEqual(["class", "href"]);
  });

  it("does not let a bare URL break out of href=\"…\"", () => {
    const a = onlyAnchor(
      renderMarkdown('See https://example.com/a"onmouseover="steal()\n'),
    );
    expect(attrNames(a)).toEqual(["class", "href", "target", "rel"]);
  });

  it("still renders ordinary links intact", () => {
    const html = renderMarkdown("[docs](https://example.com/a?x=1&y=2)");
    // `&` was entity-escaped by the first pass and must not be escaped twice.
    expect(html).toContain('href="https://example.com/a?x=1&amp;y=2"');
    expect(html).not.toContain("&amp;amp;");
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).toContain(">docs</a>");
  });

  it("still renders an in-document anchor link intact", () => {
    const html = renderMarkdown("[jump](#getting-started)");
    expect(html).toContain('href="#getting-started"');
  });

  it("keeps header slugs stable across the new quote escaping", () => {
    // The regression this guards: quotes now become entities *before*
    // `slugify` sees them, and an entity's letters would otherwise survive
    // into the id ("claude39s-setup"), silently breaking every
    // `[…](#claudes-setup)` in the document.
    expect(renderMarkdown("## Claude's setup")).toContain('id="claudes-setup"');
    expect(renderMarkdown('## The "safe" mode')).toContain('id="the-safe-mode"');
  });

  it("still refuses to emit raw tags from the source document", () => {
    const html = renderMarkdown("<img src=x onerror=alert(1)>");
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img");
  });
});
