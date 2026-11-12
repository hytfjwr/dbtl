// Parity check between the playground's selector engine and `dbtl ls`.
//
//   DBTL_BIN=/path/to/dbtl node scripts/check-selectors.mjs
//
// Each selector runs through `dbtl ls --output uids --manifest <fixture>` and through
// src/scripts/selector.ts on the same fixture, and the two sets of unique_ids must be equal.
// A selector the binary rejects (exit 2) must be rejected by the engine too.
// Exits 1 on the first run with any difference. Not part of the Pages build.
//
// The engine is TypeScript; this script loads it with Node's built-in type stripping.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(siteRoot, "..");

const dbtl = process.env.DBTL_BIN;
if (!dbtl) {
  console.error("check-selectors: set DBTL_BIN to a built dbtl binary");
  process.exit(2);
}

// Regenerate the graph data so the check never reads a stale file.
const prebuild = spawnSync(process.execPath, [path.join(siteRoot, "scripts", "prebuild.mjs")], { encoding: "utf8" });
if (prebuild.status !== 0) {
  console.error(prebuild.stderr || prebuild.stdout);
  process.exit(2);
}

const load = (rel) => import(pathToFileURL(path.join(siteRoot, "src", "scripts", rel)).href);
const { evaluate, SelectorError } = await load("selector.ts");
const { toProject } = await load("samples.ts");
const graph = JSON.parse(fs.readFileSync(path.join(siteRoot, "src", "data", "graph.json"), "utf8"));

// Selectors that make sense on any project.
const shared = [
  "*",
  "?*",
  "stg_*",
  "*_snapshot",
  "[a-d]*",
  "[!s]*",
  "[z-a]*",
  "stg_[b-a]*",
  "[!z-a]*",
  "dim_* fct_*",
  "path:staging",
  "path:models",
  "path:marts+",
  "config.materialized:table",
  "config.materialized:view",
  "config.materialized:incremental",
  "config.materialized:seed",
  "config.materialized:table,path:marts",
  "path:staging,config.materialized:view",
  "tag:nope",
  "no_such_node",
  "no_such_node+",
  "meta:owner",
  "meta:owner=alice",
  "meta:nope",
  "tag:nope,path:marts",
  // Rejected by the binary, so rejected by the engine. `x+N` is not part of the grammar:
  // a trailing hop is a bare `+` (unbounded), only a leading hop takes a number.
  "fct_orders+2",
  "foo:bar",
  "tag:",
  "path:",
  "config.materialized:",
  "config.foo:bar",
  "meta:",
  "meta:=x",
  "a,,b",
  "a,",
  "+",
  "2+",
  "x+y",
  "++a",
  "state:modified",
  "state:new+",
  "state:other",
];

const perSample = {
  jaffle_finance: [
    "fct_subscription_process",
    "+fct_subscription_process",
    "fct_subscription_process+",
    "+fct_subscription_process+",
    "1+fct_subscription_process",
    "2+fct_subscription_process",
    "3+fct_subscription_process+",
    "1+int_suppliers__combined+",
    "0+dim_suppliers",
    "dim_suppliers+",
    "stg_payment__*",
    "stg_payment__supplier?",
    "int_*__combined",
    "pos_[ct]*",
    "pos_[!c]*",
    "pos_???",
    "tag:marts",
    "tag:daily,tag:marts",
    "tag:daily,!x",
    "tag:marts+",
    "+tag:monthly",
    "tag:adhoc tag:monthly",
    "tag:utilities,config.materialized:table",
    "path:marts,tag:monthly+",
    "path:pos_prep",
    "path:staging/jaffle",
    "exposure:subscription_churn_notebook",
    "+exposure:delivery_kpi_dashboard",
    "exposure:delivery_kpi_dashboard+",
    "exposure:nope",
    "+dim_suppliers,tag:marts",
    "1+fct_*,tag:monthly",
    "stg_masterdata__*+ int_suppliers__combined",
    "budgets companies",
    "2+rpt_delivery_base_metrics+1",
  ],
  sample: [
    "fct_orders",
    "+fct_orders",
    "fct_orders+",
    "+fct_orders+",
    "1+fct_orders",
    "2+fct_orders+",
    "stg_orders+",
    "1+cll_*",
    "customers",
    "customers+",
    "+customers_snapshot",
    "*_snapshot",
    "fct_*,config.materialized:incremental",
    "config.materialized:snapshot",
    "path:marts",
    "path:marts,config.materialized:table",
    "path:country",
    "meta:owner",
    "meta:owner=alice",
    "meta:owner=bob",
    "meta:team=analytics-eng",
    "meta:domain=finance,config.materialized:table",
    "meta:shared=yes",
    "meta:shared=yes+",
    "meta:tree_src",
    "meta:deep",
    'meta:deep={"x":1}',
    "meta:pii=true",
    "meta:owner=data-eng meta:owner=alice",
    "exposure:orders_dashboard",
    "+exposure:churn_notebook",
    "exposure:churn_notebook+",
    "+orders_dashboard",
  ],
};

let failures = 0;
let checks = 0;

for (const [name, fixture] of [
  ["jaffle_finance", "manifest.json"],
  ["sample", "sample_manifest.json"],
]) {
  const project = toProject(graph[name]);
  const manifest = path.join(repoRoot, "tests", "fixtures", fixture);
  const specs = [...shared, ...perSample[name]];
  let nonEmpty = 0;
  let rejected = 0;
  for (const spec of specs) {
    checks++;
    const run = spawnSync(dbtl, ["ls", "--output", "uids", "--manifest", manifest, spec], { encoding: "utf8" });
    if (run.error || (run.status !== 0 && run.status !== 2)) {
      failures++;
      console.error(`FAIL [${name}] ${spec}\n  dbtl ls failed: exit ${run.status} ${run.error ?? run.stderr}`);
      continue;
    }
    const expected = run.status === 2 ? null : new Set(run.stdout.split("\n").filter(Boolean));
    let actual;
    try {
      actual = evaluate(project, spec);
    } catch (error) {
      if (!(error instanceof SelectorError)) throw error;
      actual = null;
    }
    if (expected === null && actual === null) {
      rejected++;
      continue;
    }
    if (expected === null || actual === null) {
      failures++;
      console.error(
        `FAIL [${name}] ${spec}\n  dbtl ${expected === null ? "rejects it" : `matches ${expected.size}`}` +
          `, engine ${actual === null ? "rejects it" : `matches ${actual.size}`}`,
      );
      continue;
    }
    const missing = [...expected].filter((x) => !actual.has(x));
    const extra = [...actual].filter((x) => !expected.has(x));
    if (missing.length || extra.length) {
      failures++;
      console.error(`FAIL [${name}] ${spec}`);
      if (missing.length) console.error(`  engine is missing ${missing.length}: ${missing.slice(0, 5).join(", ")}`);
      if (extra.length) console.error(`  engine has ${extra.length} extra: ${extra.slice(0, 5).join(", ")}`);
      continue;
    }
    if (expected.size > 0) nonEmpty++;
  }
  console.log(
    `${name}: ${specs.length} selectors, ${nonEmpty} with matches, ${rejected} rejected by both, ${specs.length - nonEmpty - rejected} empty on both`,
  );
}

if (failures > 0) {
  console.error(`check-selectors: ${failures} of ${checks} runs differ`);
  process.exit(1);
}
console.log(`check-selectors: all ${checks} runs match`);
