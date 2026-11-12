import { getCollection, type CollectionEntry } from "astro:content";
import navJson from "../nav.json";
import { referenceDescriptionFor, referenceTitle } from "./reference.ts";

export type PageKind = "docs" | "reference" | "changelog";

export interface NavPage {
  /** Route path without slashes at either end, e.g. `cli/lint`. */
  slug: string;
  title: string;
  description: string;
  kind: PageKind;
  group: string;
  /** Collection entry id, e.g. `cli/lint` (docs) or `lint-rules` (reference). */
  id: string;
  entry: CollectionEntry<"docs"> | CollectionEntry<"reference"> | CollectionEntry<"changelog">;
}

export interface NavGroup {
  group: string;
  pages: NavPage[];
}

export interface Nav {
  groups: NavGroup[];
  /** Every page in sidebar order. */
  pages: NavPage[];
}

interface RawGroup {
  group: string;
  items: string[];
}

let cached: Promise<Nav> | undefined;

/**
 * Resolve `nav.json` against both collections. The build fails when a nav
 * slug has no page, when a page is missing from the nav, or on duplicates.
 */
export function loadNav(): Promise<Nav> {
  cached ??= build();
  return cached;
}

async function build(): Promise<Nav> {
  const available = new Map<string, Omit<NavPage, "group">>();

  for (const entry of await getCollection("docs")) {
    available.set(entry.id, {
      slug: entry.id,
      title: entry.data.title,
      description: entry.data.description,
      kind: "docs",
      id: entry.id,
      entry,
    });
  }
  for (const entry of await getCollection("reference")) {
    const slug = `reference/${entry.id}`;
    const body = entry.body ?? "";
    available.set(slug, {
      slug,
      title: referenceTitle(body),
      description: referenceDescriptionFor(entry.id, body),
      kind: "reference",
      id: entry.id,
      entry,
    });
  }
  for (const entry of await getCollection("changelog")) {
    const body = entry.body ?? "";
    available.set(entry.id, {
      slug: entry.id,
      title: referenceTitle(body),
      description: referenceDescriptionFor(entry.id, body),
      kind: "changelog",
      id: entry.id,
      entry,
    });
  }

  const problems: string[] = [];
  const used = new Set<string>();
  const groups: NavGroup[] = [];

  for (const raw of navJson as RawGroup[]) {
    const pages: NavPage[] = [];
    for (const slug of raw.items) {
      const page = available.get(slug);
      if (!page) {
        problems.push(`nav.json lists "${slug}" but no page has that slug`);
        continue;
      }
      if (used.has(slug)) {
        problems.push(`nav.json lists "${slug}" more than once`);
        continue;
      }
      used.add(slug);
      pages.push({ ...page, group: raw.group });
    }
    groups.push({ group: raw.group, pages });
  }
  for (const slug of available.keys()) {
    if (!used.has(slug)) problems.push(`page "${slug}" is missing from nav.json`);
  }
  if (problems.length > 0) {
    throw new Error(`nav.json does not match the content:\n  - ${problems.join("\n  - ")}`);
  }

  return { groups, pages: groups.flatMap((g) => g.pages) };
}

export function neighbours(nav: Nav, slug: string): { prev?: NavPage; next?: NavPage } {
  const i = nav.pages.findIndex((p) => p.slug === slug);
  return { prev: nav.pages[i - 1], next: nav.pages[i + 1] };
}
