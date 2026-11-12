import nav from "../../nav.json";
import { url } from "../../lib/site.ts";

const slugs = new Set((nav as { items: string[] }[]).flatMap((group) => group.items));

/** The URL of a docs page by its nav slug. The build fails when the slug is not in the nav. */
export function docsHref(slug: string): string {
  if (!slugs.has(slug)) throw new Error(`home page links to "${slug}", which is not in nav.json`);
  return url(`${slug}/`);
}

/** The first page of the docs. */
export const firstDocsSlug: string = (nav as { items: string[] }[])[0]!.items[0]!;
