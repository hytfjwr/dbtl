import type { Blockquote, Paragraph, Root } from "mdast";
import type { VFile } from "vfile";
import { visit } from "unist-util-visit";

const KINDS = { NOTE: "note", TIP: "tip", WARNING: "warning" } as const;
const MARKER = /^\[!([A-Za-z]+)\][ \t]*\r?\n?/;

/**
 * GitHub alerts (`> [!NOTE]`, `> [!TIP]`, `> [!WARNING]`) become an
 * `<aside class="pane adm <kind>">` with a title paragraph.
 */
export function remarkAlerts() {
  return (tree: Root, file: VFile) => {
    visit(tree, "blockquote", (node: Blockquote) => {
      const first = node.children[0];
      if (first?.type !== "paragraph") return;
      const text = first.children[0];
      if (text?.type !== "text") return;
      const match = MARKER.exec(text.value);
      if (!match) return;

      const kind = KINDS[match[1]!.toUpperCase() as keyof typeof KINDS];
      if (!kind) {
        file.fail(`Unsupported alert type [!${match[1]}]`, node);
      }

      text.value = text.value.slice(match[0].length);
      if (text.value === "") first.children.shift();
      if (first.children.length === 0) node.children.shift();

      const title: Paragraph = {
        type: "paragraph",
        data: { hName: "p", hProperties: { className: ["pane-title"] } },
        children: [{ type: "text", value: kind }],
      };
      node.children.unshift(title);
      node.data = {
        hName: "aside",
        hProperties: { className: ["pane", "adm", kind] },
      };
    });
  };
}
