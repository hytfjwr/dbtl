// Builds the WebAssembly engine the docs site serves, and stages the
// sample data the playground fetches at runtime. Runs at the start of
// `pnpm run dev` / `pnpm run build` / `pnpm run check` (before prebuild).
//
//   node scripts/build-engine.mjs
//
// Outputs (all generated, all gitignored):
//   public/engine/dbtl_wasm.js + dbtl_wasm_bg.wasm (+ .d.ts) — the
//     wasm-bindgen --target web module, imported by src/engine/ at runtime
//   public/samples/<name>/manifest.json — each sample's engine input
//   public/samples/<name>/files.json — {project-relative path: text} for
//     the .yml files lint positions/excerpts read (.sql excerpts come from
//     the manifest's own raw_code — never copied)
//
// Set DBTL_SKIP_ENGINE=1 to skip everything — for site work that never
// touches the playground; the loader then reports "engine not built".
// Without it, a missing tool fails the build with what to install.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(siteRoot, "..");
const engineDir = path.join(siteRoot, "public", "engine");
const samplesDir = path.join(siteRoot, "public", "samples");

// The sample data the playground reads — the ONLY list of what gets
// copied under public/samples/. `ymlFrom` is walked for `*.yml|*.yaml`
// (deterministic order) because lint findings cite project-relative paths
// like `models/marts/schema.yml`; those texts land in `files.json`.
const SAMPLES = [
  {
    name: "jaffle_finance",
    manifest: "tests/fixtures/manifest.json",
    ymlFrom: null,
  },
  {
    name: "sample",
    manifest: "tests/fixtures/sample_manifest.json",
    ymlFrom: "tests/fixtures/sample_project",
  },
];

const fail = (msg) => {
  console.error(`build-engine: ${msg}`);
  process.exit(1);
};

const run = (cmd, args, opts = {}) => {
  const r = spawnSync(cmd, args, { encoding: "utf8", ...opts });
  if (r.error?.code === "ENOENT") return { missing: true, r };
  if (r.error) fail(`${cmd} failed to start: ${r.error.message}`);
  return { missing: false, r };
};

// The crate's pinned wasm-bindgen version — the CLI that post-processes
// the .wasm must emit the matching schema, so the two are version-locked.
function pinnedWasmBindgen() {
  const toml = fs.readFileSync(path.join(repoRoot, "crates", "wasm", "Cargo.toml"), "utf8");
  const m = /^wasm-bindgen\s*=\s*"=([0-9.]+)"/m.exec(toml);
  if (!m) fail('no `wasm-bindgen = "=x.y.z"` pin found in crates/wasm/Cargo.toml');
  return m[1];
}

function checkTools(version) {
  const problems = [];
  const cargo = run("cargo", ["--version"]);
  if (cargo.missing) {
    problems.push(
      "cargo is not on PATH — install Rust (rustup), then `rustup target add wasm32-unknown-unknown`",
    );
  } else {
    const targets = run("rustup", ["target", "list", "--installed"]);
    if (!targets.missing && !targets.r.stdout.includes("wasm32-unknown-unknown")) {
      problems.push(
        "the wasm32 target is not installed — run `rustup target add wasm32-unknown-unknown`",
      );
    }
  }
  const wbg = run("wasm-bindgen", ["--version"]);
  if (wbg.missing) {
    problems.push(
      `wasm-bindgen-cli is not on PATH — install version ${version} (the crate's pin), e.g. ` +
        `\`cargo install wasm-bindgen-cli --version ${version} --locked\``,
    );
  } else if (wbg.r.stdout.trim() !== `wasm-bindgen ${version}`) {
    problems.push(
      `wasm-bindgen-cli ${wbg.r.stdout.trim().replace(/^wasm-bindgen\s*/, "")} does not match the ` +
        `crate's pinned wasm-bindgen ${version} — the two must move together; ` +
        `install wasm-bindgen-cli ${version}`,
    );
  }
  if (problems.length > 0) {
    console.error("build-engine: the docs site's WebAssembly engine cannot be built:");
    for (const p of problems) console.error(`  - ${p}`);
    console.error("  (set DBTL_SKIP_ENGINE=1 to build the site without the engine)");
    process.exit(1);
  }
}

// project-relative path (forward slashes) of every .yml/.yaml under `dir`,
// sorted so files.json is byte-identical across runs and platforms.
function ymlFiles(dir) {
  const out = [];
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (/\.ya?ml$/.test(e.name)) out.push(p);
    }
  };
  walk(dir);
  return out.map((p) => path.relative(dir, p).split(path.sep).join("/")).sort();
}

function stageSamples() {
  fs.rmSync(samplesDir, { recursive: true, force: true });
  for (const sample of SAMPLES) {
    const out = path.join(samplesDir, sample.name);
    fs.mkdirSync(out, { recursive: true });
    fs.copyFileSync(path.join(repoRoot, sample.manifest), path.join(out, "manifest.json"));
    const files = {};
    if (sample.ymlFrom) {
      const root = path.join(repoRoot, sample.ymlFrom);
      for (const rel of ymlFiles(root)) {
        files[rel] = fs.readFileSync(path.join(root, rel), "utf8");
      }
    }
    fs.writeFileSync(path.join(out, "files.json"), `${JSON.stringify(files)}\n`);
  }
}

if (process.env.DBTL_SKIP_ENGINE === "1") {
  console.log(
    "build-engine: skipped (DBTL_SKIP_ENGINE=1) — the playground will report 'engine not built'",
  );
  process.exit(0);
}

const version = pinnedWasmBindgen();
checkTools(version);

const cargo = run(
  "cargo",
  [
    "build",
    "-p",
    "dbtl-wasm",
    "--target",
    "wasm32-unknown-unknown",
    "--profile",
    "wasm-release",
    "--locked",
  ],
  { cwd: repoRoot },
);
if (cargo.r.status !== 0) {
  process.stderr.write(cargo.r.stderr);
  fail("cargo build failed");
}

fs.rmSync(engineDir, { recursive: true, force: true });
fs.mkdirSync(engineDir, { recursive: true });
const wasm = path.join(
  repoRoot,
  "target",
  "wasm32-unknown-unknown",
  "wasm-release",
  "dbtl_wasm.wasm",
);
const bind = run("wasm-bindgen", ["--target", "web", "--out-dir", engineDir, wasm]);
if (bind.r.status !== 0) {
  process.stderr.write(bind.r.stderr);
  fail("wasm-bindgen failed");
}

stageSamples();

const bytes = fs.statSync(path.join(engineDir, "dbtl_wasm_bg.wasm")).size;
console.log(
  `build-engine: dbtl_wasm_bg.wasm ${(bytes / 1024).toFixed(0)} KiB -> public/engine/, ` +
    `${SAMPLES.length} samples -> public/samples/`,
);
