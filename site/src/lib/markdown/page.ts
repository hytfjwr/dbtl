import type { Root } from "mdast";
import type { VFile } from "vfile";
import { SKIP, visit } from "unist-util-visit";

const COMMENT = /^<!--[\s\S]*-->$/;

/** Drop HTML comments (block or inline); they never belong in the rendered page. */
export function remarkComments() {
  return (tree: Root) => {
    visit(tree, "html", (node, index, parent) => {
      if (parent && index !== undefined && COMMENT.test(node.value.trim())) {
        parent.children.splice(index, 1);
        return [SKIP, index];
      }
    });
  };
}

/**
 * Generated reference pages start with a level-1 heading that the layout already
 * renders as the title, so that heading is removed from the body.
 */
export function remarkReferencePage(options: { referenceRoot: string }) {
  return (tree: Root, file: VFile) => {
    if (!file.path || !file.path.startsWith(options.referenceRoot)) return;
    const index = tree.children.findIndex(
      (node) => node.type === "heading" && node.depth === 1,
    );
    if (index !== -1) tree.children.splice(index, 1);
  };
}
