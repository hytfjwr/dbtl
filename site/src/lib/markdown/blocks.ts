import type { Element, ElementContent, Root } from "hast";
import { SKIP, visit } from "unist-util-visit";

const LANG = /(?:^|\s)language-(\S+)/;
const HEADING = /^h[2-4]$/;
/** Tables with at least this many columns get the wide treatment. */
const WIDE_COLUMNS = 4;
/** Inline code up to this many characters never wraps; longer code may, so a narrow screen never scrolls sideways. */
const NOWRAP_CODE = 28;

function classes(node: Element): string[] {
  const c = node.properties?.className;
  return Array.isArray(c) ? c.map(String) : [];
}

function textOf(node: Element): string {
  let out = "";
  visit(node, "text", (t) => {
    out += t.value;
  });
  return out.trim();
}

function elements(node: Element, tagName: string): Element[] {
  return node.children.filter((c): c is Element => c.type === "element" && c.tagName === tagName);
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

/** A table is scrollable, so its container is a focusable, named region. */
function region(label: string, className: string[], table: Element): Element {
  return {
    type: "element",
    tagName: "div",
    properties: { className, tabIndex: 0, role: "region", ariaLabel: label || "Table" },
    children: [table],
  };
}

/**
 * A table with many columns. On a wide screen the first column stays put while
 * the rest scrolls; on a phone every row stacks into one record. Stacking turns
 * the table elements into blocks, which some browsers answer by dropping the table
 * semantics, so the roles are written out, and each cell carries its column label
 * for the CSS to print in front of it.
 */
function wideTable(table: Element, label: string): Element {
  const head = elements(table, "thead")[0];
  const labels = head ? elements(elements(head, "tr")[0] ?? head, "th").map(textOf) : [];
  const last = labels.length - 1;

  table.properties = { ...table.properties, role: "table" };
  for (const section of [...elements(table, "thead"), ...elements(table, "tbody")]) {
    section.properties = { ...section.properties, role: "rowgroup" };
    for (const row of elements(section, "tr")) {
      row.properties = { ...row.properties, role: "row" };
      let column = 0;
      for (const cell of row.children) {
        if (cell.type !== "element" || (cell.tagName !== "th" && cell.tagName !== "td")) continue;
        const props: Record<string, unknown> = { ...cell.properties };
        if (cell.tagName === "th") {
          props.role = "columnheader";
          props.scope = "col";
        } else {
          props.role = "cell";
          if (column === last) {
            props.className = [...classes(cell), "desc"];
          } else if (column > 0) {
            props.dataTh = labels[column] ?? "";
            const value = textOf(cell);
            if (value === "" || value === "—") props.dataEmpty = "";
          }
        }
        cell.properties = props as Element["properties"];
        column += 1;
      }
    }
  }

  const hint: Element = {
    type: "element",
    tagName: "p",
    properties: { className: ["tbl-hint"], ...ignored },
    children: [{ type: "text", value: `${labels.length} columns. Scroll sideways for the rest.` }],
  };
  return {
    type: "element",
    tagName: "div",
    properties: { className: ["tbl-wrap"] },
    children: [region(label, ["tbl", "wide"], table), hint],
  };
}

/**
 * Wrap every code block in a labelled figure with a copy button, and every
 * table in a horizontally scrollable region.
 *
 *   <figure class="pane code" data-lang="sh">
 *     <figcaption class="pane-title">sh</figcaption>
 *     <button class="copy" type="button">copy</button>
 *     <pre>…</pre>
 *   </figure>
 */
export function rehypeBlocks() {
  return (tree: Root) => {
    // The nearest heading names the table region that follows it.
    let heading = "";
    visit(tree, "element", (node: Element, index, parent) => {
      if (HEADING.test(node.tagName)) heading = textOf(node);
      if (!parent || index === undefined) return;

      if (node.tagName === "code" && (parent as Element).tagName !== "pre") {
        if (textOf(node).length <= NOWRAP_CODE) {
          node.properties = { ...node.properties, className: [...classes(node), "nw"] };
        }
        return;
      }

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
        const head = elements(node, "thead")[0];
        const columns = head ? elements(elements(head, "tr")[0] ?? head, "th").length : 0;
        (parent.children as ElementContent[])[index] =
          columns >= WIDE_COLUMNS
            ? wideTable(node, heading)
            : region(heading, ["tbl"], node);
        return SKIP;
      }
    });
  };
}
