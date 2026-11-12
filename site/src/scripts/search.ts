/**
 * Adapter between the full-text index (built by `pagefind` after `astro build`)
 * and the contract the search UI uses:
 *
 *   search(query) -> Promise<Array<{ url, title, section, excerpt }>>
 *
 * Pagefind is imported lazily on the first call. Besides each matching page,
 * the headings that matched inside it are returned as their own hits.
 */

export interface SearchHit {
  url: string;
  title: string;
  section: string;
  /** HTML; Pagefind escapes the text and wraps matches in <mark>. */
  excerpt: string;
}

/** The index is missing (`astro dev`, or a build that has not run pagefind yet). */
export class SearchUnavailable extends Error {
  constructor(cause?: unknown) {
    super("Search index not found. Run `pnpm run build && pnpm run preview` to search.");
    this.name = "SearchUnavailable";
    this.cause = cause;
  }
}

interface PagefindSubResult {
  title: string;
  url: string;
  excerpt: string;
}
interface PagefindData {
  url: string;
  excerpt: string;
  meta: { title?: string; section?: string };
  sub_results: PagefindSubResult[];
}
interface PagefindApi {
  options(options: { basePath: string }): Promise<void>;
  init(): Promise<void>;
  debouncedSearch(
    query: string,
    options?: Record<string, unknown>,
    wait?: number,
  ): Promise<{ results: Array<{ data(): Promise<PagefindData> }> } | null>;
}

/** Hits per page: the page itself plus up to this many headings. */
const SUB_RESULTS_PER_PAGE = 3;
const MAX_PAGES = 8;

let api: Promise<PagefindApi> | undefined;

function basePath(): string {
  const base = import.meta.env.BASE_URL;
  return `${base.endsWith("/") ? base : `${base}/`}pagefind/`;
}

function load(): Promise<PagefindApi> {
  api ??= (async () => {
    try {
      const path = basePath();
      const mod = (await import(/* @vite-ignore */ `${path}pagefind.js`)) as PagefindApi;
      await mod.options({ basePath: path });
      await mod.init();
      return mod;
    } catch (error) {
      api = undefined;
      throw new SearchUnavailable(error);
    }
  })();
  return api;
}

export async function search(query: string): Promise<SearchHit[]> {
  const q = query.trim();
  if (q === "") return [];
  const pagefind = await load();
  // debouncedSearch resolves null when a newer query superseded this one.
  const response = await pagefind.debouncedSearch(q, {}, 120);
  if (!response) return [];

  const pages = await Promise.all(response.results.slice(0, MAX_PAGES).map((r) => r.data()));
  const hits: SearchHit[] = [];
  for (const page of pages) {
    const title = page.meta.title ?? page.url;
    const section = page.meta.section ?? "";
    hits.push({ url: page.url, title, section, excerpt: page.excerpt });
    // Sub-results without an anchor describe the page itself, already added above.
    const headings = page.sub_results.filter((s) => s.url.includes("#"));
    for (const sub of headings.slice(0, SUB_RESULTS_PER_PAGE)) {
      hits.push({
        url: sub.url,
        title: `${title} › ${sub.title}`,
        section,
        excerpt: sub.excerpt,
      });
    }
  }
  return hits;
}
