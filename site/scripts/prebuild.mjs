// Generates the build-time data files under src/data/ (both gitignored).
//
//   graph.json    both sample projects' graphs, {name: {name, nodes}}, one record per node
//   version.json  the root package version, read from Cargo.toml
//
// Both inputs are read-only.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(siteRoot, "..");
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

// source.<pkg>.<source>.<table> and <type>.<pkg>.<name> both carry the package second.
const packageOf = (uid) => uid.split(".")[1] ?? "";

// The two sample projects the home demo and the playground run on.
const SAMPLES = [
  { name: "jaffle_finance", file: "manifest.json" },
  { name: "sample", file: "sample_manifest.json" },
];

// Short flags shown next to a column in the inspector, from the tests declared on it.
const TEST_FLAGS = {
  unique: "U",
  not_null: "NN",
  relationships: "R",
  accepted_values: "AV",
  accepted_range: "AR",
};

// Same stringification dbtl applies to `meta` before matching `meta:key=value`:
// strings stay as they are, everything else becomes compact JSON.
const metaStrings = (meta) =>
  Object.fromEntries(
    Object.entries(meta ?? {})
      .map(([k, v]) => [k, typeof v === "string" ? v : JSON.stringify(v)])
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
  );

function buildGraph(file) {
  const manifest = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "tests", "fixtures", file), "utf8"),
  );
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
      // dbtl reads materialization, tags and meta only from models, seeds and
      // snapshots (and meta from sources); exposures carry none of them.
      const built = ["model", "seed", "snapshot"].includes(raw.resource_type);
      return {
        uid,
        name: raw.name,
        resource_type: raw.resource_type,
        materialized: built ? (raw.config?.materialized ?? null) : null,
        tags: built ? (raw.tags ?? []) : [],
        meta: raw.resource_type === "exposure" ? {} : metaStrings(raw.meta),
        // `path:` selectors match this field (relative to the resource root), not the display path.
        selector_path: raw.path ?? "",
        path: raw.original_file_path ?? raw.path ?? "",
        package: raw.package_name ?? packageOf(uid),
        test_count: testCount.get(uid) ?? 0,
        description: (raw.description ?? "").trim(),
        columns: Object.values(raw.columns ?? {}).map((c) => [c.name, flagsFor(uid, c.name)]),
        parents: parentsOf(uid, raw),
      };
    })
    .sort((a, b) => (a.uid < b.uid ? -1 : a.uid > b.uid ? 1 : 0));
}

fs.mkdirSync(outDir, { recursive: true });
const graph = Object.fromEntries(
  SAMPLES.map(({ name, file }) => [name, { name, nodes: buildGraph(file) }]),
);
const version = readVersion();
fs.writeFileSync(path.join(outDir, "graph.json"), `${JSON.stringify(graph)}\n`);
fs.writeFileSync(path.join(outDir, "version.json"), `${JSON.stringify({ version })}\n`);
const counts = Object.values(graph)
  .map((g) => `${g.name} ${g.nodes.length}`)
  .join(", ");
console.log(`prebuild: ${counts} nodes, version ${version}`);
