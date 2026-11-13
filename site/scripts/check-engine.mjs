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

// The same two fixtures the Rust parity suite runs — names must match the
// directories build-engine.mjs staged under public/samples/.
const SAMPLES = ["jaffle_finance", "sample"];

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

// The expected lint bodies — `{text, json}` per sample — from the engine
// running natively over the fixture data.
function expected(sample) {
  const run = spawnSync(
    "cargo",
    ["run", "-q", "--locked", "-p", "dbtl-wasm", "--example", "fixture_lint", sample],
    {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  if (run.error || run.status !== 0) {
    fail(`fixture_lint ${sample} failed: ${run.error?.message ?? run.stderr}`);
  }
  return JSON.parse(run.stdout);
}

const { loadEngine } = await import("../src/engine/index.ts");
const engine = await loadEngine({ dir: engineDir });

let failures = 0;
for (const name of SAMPLES) {
  const dir = path.join(samplesDir, name);
  const manifest = fs.readFileSync(path.join(dir, "manifest.json"), "utf8");
  const files = JSON.parse(fs.readFileSync(path.join(dir, "files.json"), "utf8"));
  const load = engine.load({ manifest, files });
  if (load.nodes === 0) fail(`${name}: the engine loaded zero nodes`);
  const out = engine.lint({});
  const want = expected(name);
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
