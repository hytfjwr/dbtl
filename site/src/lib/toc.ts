import type { MarkdownHeading } from "astro";

export interface TocItem {
  slug: string;
  text: string;
  depth: 2 | 3;
}

/** The "On this page" entries: h2 and h3 headings, in document order. */
export function tocItems(headings: MarkdownHeading[]): TocItem[] {
  return headings
    .filter((h) => h.depth === 2 || h.depth === 3)
    .map((h) => ({ slug: h.slug, text: h.text, depth: h.depth as 2 | 3 }));
}
