import version from "../data/version.json";

const base = import.meta.env.BASE_URL;

/** Build an internal URL from the configured base. `path` has no leading slash. */
export function url(path = ""): string {
  return `${base.endsWith("/") ? base : `${base}/`}${path}`;
}

export const REPO_URL = "https://github.com/hytfjwr/dbtl";
export const VERSION: string = version.version;

export function editUrl(id: string): string {
  return `${REPO_URL}/edit/main/site/src/content/docs/${id}.md`;
}
