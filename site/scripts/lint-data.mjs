// Regenerates the lint results the playground's "Lint findings" tab shows.
//
//   DBTL_BIN=/path/to/dbtl node scripts/lint-data.mjs
//
// Runs `dbtl lint --format json` over each sample fixture and keeps what the tab needs in
// src/data/lint-<sample>.json. Those files are committed; rerun this when the rules or the
// fixtures change. `dbtl lint` exits 1 when it has error findings, which is a normal result here.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(siteRoot, "..");

const dbtl = process.env.DBTL_BIN;
if (!dbtl) {
  console.error("lint-data: set DBTL_BIN to a built dbtl binary");
  process.exit(2);
}

const SAMPLES = [
  { name: "jaffle_finance", file: "manifest.json" },
  { name: "sample", file: "sample_manifest.json" },
];

for (const { name, file } of SAMPLES) {
  const manifest = path.join(repoRoot, "tests", "fixtures", file);
  const run = spawnSync(dbtl, ["lint", "--format", "json", "--manifest", manifest], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (run.error || (run.status !== 0 && run.status !== 1)) {
    console.error(
      `lint-data: dbtl lint failed on ${file}: exit ${run.status} ${run.error ?? run.stderr}`,
    );
    process.exit(1);
  }
  const report = JSON.parse(run.stdout);
  if (report.schema !== "dbtl.report/v1") {
    console.error(`lint-data: unexpected report schema ${report.schema}`);
    process.exit(1);
  }

  const gates = report.gates.filter((g) => g.id.startsWith("lint:"));
  const findings = gates.flatMap((gate) =>
    (gate.findings ?? []).map((f) => ({
      rule: f.rule,
      severity: f.severity,
      uid: f.node_uid,
      message: f.message,
      file: f.location?.file ?? "",
    })),
  );
  const data = {
    project: report.project,
    // Rules that need a warehouse catalog are skipped; they did not run.
    rules: gates.filter((g) => g.status !== "skip").length,
    findings,
  };
  const out = path.join(siteRoot, "src", "data", `lint-${name}.json`);
  fs.writeFileSync(out, `${JSON.stringify(data, null, 2)}\n`);
  console.log(
    `lint-data: ${name}: ${findings.length} findings, ${data.rules} rules -> ${path.relative(repoRoot, out)}`,
  );
}
