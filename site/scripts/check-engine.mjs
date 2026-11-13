// Verifies the generated wasm engine produces the same lint output the
// native engine does — for both bundled samples, byte for byte. Runs as
// part of `pnpm run build` (after astro build) and standalone via
// `pnpm run check:engine`.
//
//   node scripts/check-engine.mjs
//
// The wasm module is loaded through the same src/engine/ loader the
// browser uses (Node takes its initSync path). The expected bytes come
// from `cargo run -p dbtl-wasm --example fixture_lint` — the same pure
// engine linked natively, so this checks the wasm artifact end to end:
// compile, bindings, loader, and the staged sample data in public/samples/.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(siteRoot, "..");
const engineDir = path.join(siteRoot, "public", "engine");
const samplesDir = path.join(siteRoot, "public", "samples");

// The samples build-engine.mjs staged — whatever directories hold a
// manifest.json + files.json pair get checked, so the two scripts never
// keep separate lists.
function stagedSamples() {
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

const fail = (msg) => {
  console.error(`check-engine: ${msg}`);
  process.exit(1);
};

if (process.env.DBTL_SKIP_ENGINE === "1") {
  console.log("check-engine: skipped (DBTL_SKIP_ENGINE=1)");
  process.exit(0);
}
if (!fs.existsSync(path.join(engineDir, "dbtl_wasm.js"))) {
  fail(`${engineDir} has no generated module — run scripts/build-engine.mjs first`);
}
const SAMPLES = stagedSamples();
if (SAMPLES.length === 0) {
  fail(`${samplesDir} has no staged samples — run scripts/build-engine.mjs first`);
}

// The staged directories must be exactly the projects the switcher offers —
// pg-render.ts and build-engine.mjs keep no shared constant, so a drift
// fails here instead of 404ing a fetch in the browser.
const { SAMPLES: offered } = await import("../src/scripts/pg-render.ts");
const want = [...offered].sort();
if (SAMPLES.length !== want.length || SAMPLES.some((n, i) => n !== want[i])) {
  fail(
    `staged samples [${SAMPLES.join(", ")}] do not match the switcher's ` +
      `SAMPLES [${want.join(", ")}] — update both lists together`,
  );
}

// The expected lint bodies — `{text, json}` per sample — from the engine
// running natively over the same staged files the wasm load reads.
function expected(dir) {
  const run = spawnSync(
    "cargo",
    [
      "run",
      "-q",
      "--locked",
      "-p",
      "dbtl-wasm",
      "--example",
      "fixture_lint",
      path.join(dir, "manifest.json"),
      path.join(dir, "files.json"),
    ],
    {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  if (run.error || run.status !== 0) {
    fail(`fixture_lint ${dir} failed: ${run.error?.message ?? run.stderr}`);
  }
  return JSON.parse(run.stdout);
}

// The loader is TypeScript — imported directly, which needs Node's type
// stripping (on by default in Node ≥ 23.6 and ≥ 22.18; the site floor,
// 22.23.4, covers it — older releases fail the engines check anyway).
let loadEngine;
try {
  ({ loadEngine } = await import("../src/engine/index.ts"));
} catch (e) {
  if (
    e &&
    (e.code === "ERR_UNKNOWN_FILE_EXTENSION" || e.code === "ERR_UNSUPPORTED_ESM_URL_SCHEME")
  ) {
    fail(
      `importing src/engine/index.ts needs Node's TypeScript type stripping ` +
        `(Node ≥ 23.6 or ≥ 22.18); this Node is ${process.version}`,
    );
  }
  throw e;
}
const engine = await loadEngine({ dir: engineDir });

let failures = 0;
for (const name of SAMPLES) {
  const dir = path.join(samplesDir, name);
  const manifest = fs.readFileSync(path.join(dir, "manifest.json"), "utf8");
  const files = JSON.parse(fs.readFileSync(path.join(dir, "files.json"), "utf8"));
  const load = engine.load({ manifest, files });
  if (load.nodes === 0) fail(`${name}: the engine loaded zero nodes`);
  const out = engine.lint({});
  const want = expected(dir);
  for (const key of ["text", "json"]) {
    if (out[key] !== want[key]) {
      console.error(
        `check-engine: ${name}: lint ${key} differs (wasm ${out[key].length}B vs native ${want[key].length}B)`,
      );
      failures += 1;
    }
  }
  if (failures === 0) console.log(`check-engine: ${name}: wasm lint output matches native`);
}
if (failures > 0) process.exit(1);
