// Generates the build-time data files under src/data/ (all gitignored).
//
//   graph.json         both sample projects' graphs, {name: {name, nodes}}, one record per node
//   home-lineage.json  the home demo's lineage layouts — one engine lineage()
//                      call per node of the demo project — so the browser can
//                      repaint a re-root without shipping the wasm
//   version.json       the root package version, read from Cargo.toml
//
// All inputs are read-only. Under DBTL_SKIP_ENGINE (or a missing engine
// build) home-lineage.json is still written — with built:false, so the demo
// falls back to a notice instead of a broken panel.
import fs from "node:fs";
import path from "node:path";
import { engineDir, repoRoot, SAMPLES, siteRoot } from "./shared.mjs";

const outDir = path.join(siteRoot, "src", "data");

function readVersion() {
  const toml = fs.readFileSync(path.join(repoRoot, "Cargo.toml"), "utf8");
  const section = (name) => {
    const start = toml.indexOf(`[${name}]`);
    if (start === -1) return "";
    const rest = toml.slice(start + name.length + 2);
    const end = rest.search(/^\[/m);
    return end === -1 ? rest : rest.slice(0, end);
  };
  const literal = /^version\s*=\s*"([^"]+)"/m;
  // The root package may inherit its version from the workspace table.
  const found = literal.exec(section("package")) ?? literal.exec(section("workspace.package"));
  if (!found) throw new Error("prebuild: no version found in Cargo.toml");
  return found[1];
}

// The two sample projects the home demo and the playground run on — names
// and manifest paths come from the shared list in samples.ts.

// Short flags shown next to a column in the inspector, from the tests declared on it.
const TEST_FLAGS = {
  unique: "U",
  not_null: "NN",
  relationships: "R",
  accepted_values: "AV",
  accepted_range: "AR",
};

function buildGraph(manifestPath) {
  const manifest = JSON.parse(fs.readFileSync(path.join(repoRoot, manifestPath), "utf8"));
  const nodes = manifest.nodes ?? {};

  // Tests are counted against the node they are declared on, never emitted.
  // Singular tests have no attached node, so they count against every dependency.
  const testCount = new Map();
  const columnFlags = new Map();
  for (const test of Object.values(nodes)) {
    if (test.resource_type !== "test") continue;
    const targets = test.attached_node ? [test.attached_node] : (test.depends_on?.nodes ?? []);
    for (const target of targets) testCount.set(target, (testCount.get(target) ?? 0) + 1);
    const flag = TEST_FLAGS[test.test_metadata?.name];
    if (flag && test.attached_node && test.column_name) {
      const key = `${test.attached_node}\u0000${test.column_name}`;
      const set = columnFlags.get(key) ?? new Set();
      set.add(flag);
      columnFlags.set(key, set);
    }
  }
  const flagsFor = (uid, column) => {
    const set = columnFlags.get(`${uid}\u0000${column}`);
    return set
      ? Object.values(TEST_FLAGS)
          .filter((f) => set.has(f))
          .join(" ")
      : "";
  };

  const kept = [];
  const add = (uid, raw) => kept.push([uid, raw]);
  for (const [uid, raw] of Object.entries(nodes)) {
    if (["model", "seed", "snapshot"].includes(raw.resource_type)) add(uid, raw);
  }
  for (const [uid, raw] of Object.entries(manifest.sources ?? {})) add(uid, raw);
  for (const [uid, raw] of Object.entries(manifest.exposures ?? {})) add(uid, raw);

  const keptIds = new Set(kept.map(([uid]) => uid));
  const parentsOf = (uid, raw) => {
    const fromMap = manifest.parent_map?.[uid] ?? [];
    const fromDeps = raw.depends_on?.nodes ?? [];
    return [...new Set([...fromMap, ...fromDeps])].filter((p) => keptIds.has(p)).sort();
  };

  return kept
    .map(([uid, raw]) => {
      // dbtl reads materialization and tags only from models, seeds and
      // snapshots; exposures carry none of them.
      const built = ["model", "seed", "snapshot"].includes(raw.resource_type);
      return {
        uid,
        name: raw.name,
        resource_type: raw.resource_type,
        materialized: built ? (raw.config?.materialized ?? null) : null,
        tags: built ? (raw.tags ?? []) : [],
        // The home demo groups models by the first directory of their path.
        layer: raw.resource_type === "model" ? (raw.path ?? "").split("/")[0] : "",
        path: raw.original_file_path ?? raw.path ?? "",
        test_count: testCount.get(uid) ?? 0,
        description: (raw.description ?? "").trim(),
        columns: Object.values(raw.columns ?? {}).map((c) => [c.name, flagsFor(uid, c.name)]),
        parents: parentsOf(uid, raw),
      };
    })
    .sort((a, b) => (a.uid < b.uid ? -1 : a.uid > b.uid ? 1 : 0));
}

// The home demo's lineage layouts: the same engine the playground drives,
// run once per node at build time — the depth-1 neighbourhood of every
// possible re-root target, laid out by `lineage()`. The demo's browser code
// fetches this file instead of computing (or re-implementing) a layout.
async function buildHomeLineage(graph) {
  const { HOME } = await import("../src/scripts/home-view.ts");
  let engine;
  try {
    const { loadEngine } = await import("../src/engine/index.ts");
    engine = await loadEngine({ dir: engineDir });
  } catch (e) {
    if (e?.name === "EngineNotBuiltError") return { built: false, layouts: {} };
    throw e;
  }
  const sample = SAMPLES.find((s) => s.name === HOME.project);
  const manifest = fs.readFileSync(path.join(repoRoot, sample.manifest), "utf8");
  engine.load({ manifest });
  const layouts = {};
  for (const n of graph[HOME.project].nodes) {
    const out = engine.lineage({
      root: n.uid,
      direction: HOME.direction,
      depth: HOME.depth,
      glyphs: HOME.glyphs,
      density: HOME.density,
    });
    layouts[n.uid] = {
      w: out.width,
      h: out.height,
      rows: out.rows,
      kinds: out.kinds,
      classes: out.classes,
      rects: out.rects,
      edges: out.edges,
      cell_edges: out.cell_edges,
    };
  }
  engine.dispose();
  return { built: true, layouts };
}

fs.mkdirSync(outDir, { recursive: true });
const graph = Object.fromEntries(
  SAMPLES.map(({ name, manifest }) => [name, { name, nodes: buildGraph(manifest) }]),
);
const version = readVersion();
const homeLineage = await buildHomeLineage(graph);
fs.writeFileSync(path.join(outDir, "graph.json"), `${JSON.stringify(graph)}\n`);
fs.writeFileSync(path.join(outDir, "version.json"), `${JSON.stringify({ version })}\n`);
fs.writeFileSync(path.join(outDir, "home-lineage.json"), `${JSON.stringify(homeLineage)}\n`);
const counts = Object.values(graph)
  .map((g) => `${g.name} ${g.nodes.length}`)
  .join(", ");
const lineage = homeLineage.built
  ? `, home lineage ${Object.keys(homeLineage.layouts).length} layouts`
  : " — engine not built, home demo degraded";
console.log(`prebuild: ${counts} nodes, version ${version}${lineage}`);
