// Sample projects: the build-time graph data (src/data/graph.json, written by
// scripts/prebuild.mjs) turned into the shapes the lineage renderer, the selector
// engine and the demo panels read. No DOM and no JSON import, so Node can load it too.
import type { Graph } from "./graph.ts";
import type { LineageNode } from "./lineage.ts";
import type { SelectorNode } from "./selector.ts";

export interface RawNode {
  uid: string;
  name: string;
  resource_type: string;
  materialized: string | null;
  tags: string[];
  meta: Record<string, string>;
  selector_path: string;
  path: string;
  package: string;
  test_count: number;
  description: string;
  columns: [string, string][];
  parents: string[];
}

export interface RawSample {
  name: string;
  nodes: RawNode[];
}

export interface ProjectNode extends SelectorNode, LineageNode {
  /** First directory of the model path (staging, marts, ...). Empty for other resource types. */
  layer: string;
  /** File the node is declared in, relative to the project. */
  path: string;
  desc: string;
  cols: [string, string][];
}

export interface Project extends Graph<ProjectNode> {
  name: string;
}

export interface LintFinding {
  rule: string;
  severity: "error" | "warn";
  uid: string;
  message: string;
  file: string;
}

export interface LintData {
  project: string;
  /** Rules that ran; rules that need a warehouse catalog are skipped. */
  rules: number;
  findings: LintFinding[];
}

/** The tag drawn in a node's top border. Same rule as dbtl's lineage boxes. */
export function tagOf(resourceType: string, materialized: string | null): string {
  switch (resourceType) {
    case "source":
      return "Source";
    case "seed":
      return "Seed";
    case "snapshot":
      return "Snapshot";
    case "exposure":
      return "Exposure";
    case "model":
      switch (materialized) {
        case "table":
          return "Table";
        case "view":
          return "View";
        case "incremental":
          return "Incremental";
        case "ephemeral":
          return "Ephemeral";
        default:
          return "Model";
      }
    default:
      return resourceType;
  }
}

export function toProject(raw: RawSample): Project {
  const nodes: ProjectNode[] = raw.nodes.map((n) => ({
    id: n.uid,
    name: n.name,
    type: n.resource_type,
    mat: n.materialized,
    tags: n.tags,
    selPath: n.selector_path,
    meta: n.meta,
    tag: tagOf(n.resource_type, n.materialized),
    tests: n.test_count,
    layer: n.resource_type === "model" ? n.selector_path.split("/")[0] : "",
    path: n.path,
    desc: n.description,
    cols: n.columns,
  }));
  const edges: [string, string][] = raw.nodes.flatMap((n) => n.parents.map((p): [string, string] => [p, n.uid]));
  return { name: raw.name, nodes, edges };
}

/** Test coverage as dbtl's status line counts it: models, seeds and snapshots with at least one test. */
export function coverage(p: Project): number {
  const built = p.nodes.filter((n) => n.type === "model" || n.type === "seed" || n.type === "snapshot");
  if (built.length === 0) return 0;
  return Math.round((100 * built.filter((n) => n.tests > 0).length) / built.length);
}
