import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import { unified } from "@astrojs/markdown-remark";
import sitemap from "@astrojs/sitemap";
import {
  rehypeBlocks,
  rehypeHeadings,
  remarkAlerts,
  remarkChangelog,
  remarkComments,
  remarkLinks,
  remarkLiteralTags,
  remarkReferencePage,
  remarkVersion,
} from "./src/lib/markdown/index.ts";

const BASE = "/dbtl";
const siteRoot = fileURLToPath(new URL("./", import.meta.url));

// Both collections are read from disk by the plugins below, so they need the
// absolute locations of the two content trees and of the repository root.
const linkOptions = {
  base: `${BASE}/`,
  docsRoot: fileURLToPath(new URL("./src/content/docs/", import.meta.url)),
  referenceRoot: fileURLToPath(new URL("../docs/reference/", import.meta.url)),
  repoRoot: fileURLToPath(new URL("../", import.meta.url)),
  githubBlob: "https://github.com/hytfjwr/dbtl/blob/main/",
};

const changelogFile = fileURLToPath(new URL("../CHANGELOG.md", import.meta.url));

export default defineConfig({
  site: "https://hytfjwr.github.io",
  base: BASE,
  output: "static",
  trailingSlash: "always",
  build: { format: "directory" },
  devToolbar: { enabled: false },
  integrations: [
    sitemap({
      // The 404 page and the /docs/ redirect hop are not content.
      filter: (page) => !page.includes("/404") && new URL(page).pathname !== `${BASE}/docs/`,
    }),
  ],
  vite: {
    server: { fs: { allow: [siteRoot, linkOptions.repoRoot] } },
  },
  markdown: {
    syntaxHighlight: {
      type: "shiki",
      // Plain-text fences stay unhighlighted; the block plugin still wraps them.
      excludeLangs: ["math", "text", "plaintext"],
    },
    // Token colors are CSS variables (--astro-code-*), mapped to the site's
    // --code-* tokens in styles/tokens.css so light and dark follow data-theme.
    shikiConfig: { theme: "css-variables" },
    processor: unified({
      smartypants: false,
      remarkPlugins: [
        remarkComments,
        remarkLiteralTags,
        remarkVersion,
        [remarkReferencePage, { referenceRoot: linkOptions.referenceRoot }],
        [remarkChangelog, { file: changelogFile }],
        remarkAlerts,
        [remarkLinks, linkOptions],
      ],
      rehypePlugins: [rehypeHeadings, rehypeBlocks],
    }),
  },
});
