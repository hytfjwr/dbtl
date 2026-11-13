// Shared bits for the build scripts in this directory: where the site and the
// repo roots are, where generated assets land, the one failure exit, and the
// canonical sample list (re-exported from samples.ts — a .ts so the browser
// bundle can read the same data without the node builtins this module uses).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { SAMPLES } from "./samples.ts";

export const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const repoRoot = path.resolve(siteRoot, "..");
export const engineDir = path.join(siteRoot, "public", "engine");
export const samplesDir = path.join(siteRoot, "public", "samples");

export { SAMPLES };

/** A `fail(msg)` bound to a log prefix: prints `prefix: msg` and exits 1. */
export const fail = (prefix) => (msg) => {
  console.error(`${prefix}: ${msg}`);
  process.exit(1);
};

/** Names of the samples currently staged under public/samples/, sorted. */
export function stagedSamples() {
  if (!fs.existsSync(samplesDir)) return [];
  return fs
    .readdirSync(samplesDir, { withFileTypes: true })
    .filter(
      (e) =>
        e.isDirectory() &&
        fs.existsSync(path.join(samplesDir, e.name, "manifest.json")) &&
        fs.existsSync(path.join(samplesDir, e.name, "files.json")),
    )
    .map((e) => e.name)
    .sort();
}
