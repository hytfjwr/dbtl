import type { Nodes, Root } from "mdast";
import type { VFile } from "vfile";
import { visit } from "unist-util-visit";

function textOf(node: Nodes): string {
  let out = "";
  visit(node, "text", (t) => {
    out += t.value;
  });
  return out;
}

/**
 * The repository changelog renders as its own page:
 *  - the level-1 heading is removed from the body (the layout prints the title);
 *  - an `## [Unreleased]` section with no entries is dropped — the file keeps
 *    the heading empty right after a release, where it would render a bare
 *    line above the newest version. A section holds no entries when nothing
 *    but headings and link definitions sit between it and the next `##`.
 */
export function remarkChangelog(options: { file: string }) {
  return (tree: Root, file: VFile) => {
    if (file.path !== options.file) return;

    const h1 = tree.children.findIndex((node) => node.type === "heading" && node.depth === 1);
    if (h1 !== -1) tree.children.splice(h1, 1);

    const start = tree.children.findIndex(
      (node) => node.type === "heading" && node.depth === 2 && textOf(node) === "Unreleased",
    );
    if (start === -1) return;
    let end = tree.children.length;
    for (let i = start + 1; i < tree.children.length; i++) {
      const node = tree.children[i]!;
      if (node.type === "heading" && node.depth <= 2) {
        end = i;
        break;
      }
    }
    const empty = tree.children
      .slice(start + 1, end)
      .every((node) => node.type === "heading" || node.type === "definition");
    if (empty) tree.children.splice(start, end - start);
  };
}
