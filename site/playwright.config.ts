import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";

// The suite runs against the built site, served with the same base path it is
// deployed under. It never builds — run `pnpm run build` first.
const root = path.dirname(fileURLToPath(import.meta.url));
if (!fs.existsSync(path.join(root, "dist", "index.html"))) {
  throw new Error("site/dist is missing. Run `pnpm run build` before `pnpm run test:e2e`.");
}

const port = 4377;
const origin = `http://localhost:${port}`;

export default defineConfig({
  testDir: "tests/e2e",
  reporter: process.env.CI ? [["github"], ["list"]] : "list",
  use: {
    baseURL: origin,
    // Assertions measure layout geometry, so motion stays off; every wait is on
    // an element or a document state, never on a clock.
    reducedMotion: "reduce",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
  webServer: {
    command: `pnpm exec astro preview --port ${port}`,
    url: `${origin}/dbtl/`,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
