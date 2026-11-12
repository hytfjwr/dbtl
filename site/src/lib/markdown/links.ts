import fs from "node:fs";
import path from "node:path";
import type { Root } from "mdast";
import type { VFile } from "vfile";
import { visit } from "unist-util-visit";

export interface LinkOptions {
  /** Site base with a trailing slash, e.g. `/dbtl/`. */
  base: string;
  /** Absolute directory of the hand-written pages. */
  docsRoot: string;
  /** Absolute directory of the generated reference pages. */
  referenceRoot: string;
  /** Absolute repository root, for links that leave both trees. */
  repoRoot: string;
  /** GitHub blob URL prefix, with a trailing slash. */
  githubBlob: string;
}

const SCHEME = /^[a-zA-Z][a-zA-Z0-9+.-]*:/;

function inside(root: string, file: string): boolean {
  const rel = path.relative(root, file);
  return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel);
}

function posix(p: string): string {
  return p.split(path.sep).join("/");
}

/**
 * Rewrite relative links.
 *  - a `.md` file inside either content tree becomes that page's route;
 *  - any other relative path becomes a GitHub URL on `main`;
 *  - absolute URLs, root-relative paths and `#anchors` are left alone.
 * A target that does not exist fails the build.
 */
export function remarkLinks(options: LinkOptions) {
  const { base, docsRoot, referenceRoot, repoRoot, githubBlob } = options;

  function rewrite(url: string, from: string, file: VFile, node: unknown): string {
    if (url === "" || url.startsWith("#") || url.startsWith("/") || SCHEME.test(url)) {
      return url;
    }
    const cut = url.search(/[#?]/);
    const target = decodeURI(cut === -1 ? url : url.slice(0, cut));
    const suffix = cut === -1 ? "" : url.slice(cut);
    const resolved = path.resolve(path.dirname(from), target);

    if (resolved.endsWith(".md")) {
      if (inside(referenceRoot, resolved)) {
        if (!fs.existsSync(resolved)) file.fail(`Broken link: ${url}`, node as never);
        return `${base}reference/${path.basename(resolved, ".md")}/${suffix}`;
      }
      if (inside(docsRoot, resolved)) {
        const id = posix(path.relative(docsRoot, resolved)).replace(/\.md$/, "");
        if (fs.existsSync(resolved)) return `${base}${id}/${suffix}`;
        // Hand-written pages reach the generated pages as `../reference/<name>.md`.
        const stem = id.replace(/^reference\//, "");
        if (id.startsWith("reference/") && fs.existsSync(path.join(referenceRoot, `${stem}.md`))) {
          return `${base}reference/${stem}/${suffix}`;
        }
        file.fail(`Broken link: ${url}`, node as never);
      }
    }

    const rel = posix(path.relative(repoRoot, resolved));
    if (rel.startsWith("..") || !fs.existsSync(resolved)) {
      file.fail(`Broken link: ${url}`, node as never);
    }
    return `${githubBlob}${rel}${suffix}`;
  }

  return (tree: Root, file: VFile) => {
    const from = file.path;
    if (!from) return;
    visit(tree, ["link", "definition"], (node) => {
      if (node.type === "link" || node.type === "definition") {
        node.url = rewrite(node.url, from, file, node);
      }
    });
  };
}
