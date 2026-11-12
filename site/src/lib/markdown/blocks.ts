import type { Element, ElementContent, Root } from "hast";
import { SKIP, visit } from "unist-util-visit";

const LANG = /(?:^|\s)language-(\S+)/;

function classes(node: Element): string[] {
  const c = node.properties?.className;
  return Array.isArray(c) ? c.map(String) : [];
}

function languageOf(pre: Element): string {
  const tagged = pre.properties?.dataLanguage;
  if (typeof tagged === "string" && tagged !== "plaintext") return tagged;
  const code = pre.children.find(
    (c): c is Element => c.type === "element" && c.tagName === "code",
  );
  const match = code ? LANG.exec(classes(code).join(" ")) : null;
  return match?.[1] ?? "text";
}

const ignored = { dataPagefindIgnore: "" };

/**
 * Wrap every code block in a labelled figure with a copy button, and every
 * table in a horizontally scrollable container.
 *
 *   <figure class="pane code" data-lang="sh">
 *     <figcaption class="pane-title">sh</figcaption>
 *     <button class="copy" type="button">copy</button>
 *     <pre>…</pre>
 *   </figure>
 */
export function rehypeBlocks() {
  return (tree: Root) => {
    visit(tree, "element", (node: Element, index, parent) => {
      if (!parent || index === undefined) return;

      if (node.tagName === "pre") {
        const lang = languageOf(node);
        node.properties ??= {};
        node.properties.tabIndex = 0;
        const figure: Element = {
          type: "element",
          tagName: "figure",
          properties: { className: ["pane", "code"], dataLang: lang },
          children: [
            {
              type: "element",
              tagName: "figcaption",
              properties: { className: ["pane-title"], ...ignored },
              children: [{ type: "text", value: lang }],
            },
            {
              type: "element",
              tagName: "button",
              properties: {
                className: ["copy"],
                type: "button",
                ariaLabel: `Copy ${lang} code`,
                ...ignored,
              },
              children: [{ type: "text", value: "copy" }],
            },
            node,
          ],
        };
        (parent.children as ElementContent[])[index] = figure;
        return SKIP;
      }

      if (node.tagName === "table") {
        const wrap: Element = {
          type: "element",
          tagName: "div",
          properties: { className: ["tbl"], tabIndex: 0 },
          children: [node],
        };
        (parent.children as ElementContent[])[index] = wrap;
        return SKIP;
      }
    });
  };
}
