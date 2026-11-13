import type { Element, Root } from "hast";
import GithubSlugger from "github-slugger";
import { visit } from "unist-util-visit";

const HEADING = /^h([1-6])$/;

function textOf(node: Element | Root): string {
  let out = "";
  visit(node, "text", (t) => {
    out += t.value;
  });
  return out;
}

// Pagefind reads this attribute and ranks the element's text above body copy.
// The defaults (h1 7 → h6 2) are pushed further apart so a heading match wins
// over incidental mentions in prose.
const WEIGHT: Record<number, number> = { 1: 10, 2: 9, 3: 8, 4: 7, 5: 6, 6: 5 };

/**
 * Give every heading an id using the same rules GitHub uses (so links such as
 * `#rule_id` in generated pages resolve), weight it above body text for the
 * search index, and add a hover anchor link to h2-h4. The link has no text of
 * its own; its marker is drawn with CSS.
 */
export function rehypeHeadings() {
  return (tree: Root) => {
    const slugger = new GithubSlugger();
    visit(tree, "element", (node: Element) => {
      const match = HEADING.exec(node.tagName);
      if (!match) return;
      const depth = Number(match[1]);
      const text = textOf(node);
      node.properties ??= {};
      node.properties.dataPagefindWeight = String(WEIGHT[depth]);
      if (typeof node.properties.id !== "string") {
        node.properties.id = slugger.slug(text);
      }
      if (depth >= 2 && depth <= 4) {
        node.children.unshift({
          type: "element",
          tagName: "a",
          properties: {
            className: ["anchor"],
            href: `#${node.properties.id}`,
            ariaLabel: `Link to ${text}`,
            dataPagefindIgnore: "",
          },
          children: [],
        });
      }
    });
  };
}
