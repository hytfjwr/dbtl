import type { Root } from "mdast";
import { visit } from "unist-util-visit";

// Tags a page may use on purpose. Anything else written as a bare tag in running
// text (`<dir>`, `<name>`) is a placeholder meant to be read, not an element.
const ALLOWED = new Set([
  "a", "abbr", "b", "br", "code", "details", "em", "i", "img", "kbd", "mark", "span", "strong", "sub", "summary", "sup", "u", "wbr",
]);
const TAG = /^<\/?([A-Za-z][A-Za-z0-9-]*)(?:\s[^>]*)?\/?>$/;

/**
 * Inline HTML for a tag outside the allowed set becomes literal text. Without this
 * the browser swallows `<dir>` as an unknown element and the text after it is
 * laid out inside that element.
 */
export function remarkLiteralTags() {
  return (tree: Root) => {
    visit(tree, "html", (node, index, parent) => {
      // Block-level HTML (a direct child of the page) is left alone.
      if (!parent || index === undefined || parent.type === "root") return;
      const match = TAG.exec(node.value.trim());
      if (!match || ALLOWED.has(match[1]!.toLowerCase())) return;
      (parent.children as Array<{ type: string; value?: string }>)[index] = { type: "text", value: node.value };
    });
  };
}
