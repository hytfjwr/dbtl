import fs from "node:fs";
import type { Root } from "mdast";
import type { VFile } from "vfile";
import { visit } from "unist-util-visit";

// Read with fs rather than imported: the Astro config loads this file in plain
// Node, which rejects a JSON import without an attribute. The file is written by
// the prebuild step, so it is read when a page is first processed, not at load.
const DATA = new URL("../../data/version.json", import.meta.url);
const KNOWN = /@DBTL_(?:TAG|VERSION)@/g;
// Anything shaped like a token that is not one of ours is a typo.
const ANY = /@DBTL_[A-Z_]+@/;

/**
 * Replace the release tokens in page text, code, link targets and image sources:
 * `@DBTL_TAG@` becomes `v<version>` and `@DBTL_VERSION@` becomes `<version>`, both
 * read from Cargo.toml by the prebuild step. A token-shaped string that is not
 * one of those fails the build, so a misspelled token cannot reach the site.
 */
export function remarkVersion() {
  let tokens: Record<string, string> | undefined;
  const replace = (value: string) => value.replace(KNOWN, (token) => tokens![token]!);

  return (tree: Root, file: VFile) => {
    if (!tokens) {
      const { version } = JSON.parse(fs.readFileSync(DATA, "utf8")) as { version: string };
      tokens = { "@DBTL_TAG@": `v${version}`, "@DBTL_VERSION@": version };
    }
    visit(tree, (node) => {
      if (node.type === "text" || node.type === "inlineCode" || node.type === "code") {
        node.value = replace(node.value);
        if (ANY.test(node.value)) file.fail(`Unknown version token: ${ANY.exec(node.value)![0]}`, node);
      } else if (node.type === "link" || node.type === "image") {
        node.url = replace(node.url);
        if (ANY.test(node.url)) file.fail(`Unknown version token: ${ANY.exec(node.url)![0]}`, node);
      }
    });
  };
}
