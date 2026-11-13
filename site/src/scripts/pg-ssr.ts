// The playground's first state, produced at build/dev-render time by the real
// engine running in Node: the staged jaffle_finance sample, the first example
// selector, its match list, the engine's lineage layout and the lint report.
// The browser client re-runs the same calls in a worker and swaps the markup
// in place — identical functions, so nothing flickers.
import fss from "node:fs";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  loadEngine,
  type LineageOutput,
  type LintOutput,
  type SelectedNode,
} from "../engine/index.ts";
import { EXAMPLES, SAMPLES } from "./pg-render.ts";

export interface PgInitial {
  /** false when public/engine or the staged samples are missing (DBTL_SKIP_ENGINE). */
  built: boolean;
  /** Why built is false — shown in the status row. */
  reason: string;
  project: string;
  spec: string;
  /** Total nodes in the loaded project. */
  total: number;
  /** Warnings the load emitted — the lint footer's `warning:` lines. */
  warnings: string[];
  matched: SelectedNode[];
  /** A malformed selector's message, like the `dbtl ls` exit-2 error. */
  selectError: string;
  lineage: LineageOutput | null;
  lint: LintOutput | null;
  /** A thrown lint error (bad selector or config), like the CLI's stderr. */
  lintError: string;
}

// A directory under site/public/: resolved from this file when it runs from
// the source tree (dev, scripts), and from the cwd — the site dir every script
// runs from — when the bundled SSR module's import.meta.url points elsewhere.
const publicDir = (sub: string): string => {
  const src = fileURLToPath(new URL(`../../public/${sub}`, import.meta.url));
  return fss.existsSync(src) ? src : path.resolve("public", sub);
};

export async function sampleInput(
  name: string,
): Promise<{ manifest: string; files: Record<string, string> }> {
  const dir = `${publicDir("samples")}/${name}/`;
  const [manifest, files] = await Promise.all([
    fs.readFile(`${dir}manifest.json`, "utf8"),
    fs.readFile(`${dir}files.json`, "utf8"),
  ]);
  return { manifest, files: JSON.parse(files) as Record<string, string> };
}

const message = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export async function initialState(): Promise<PgInitial> {
  const project = SAMPLES[0];
  const spec = EXAMPLES[project]![0]!;
  const base = {
    built: true,
    reason: "",
    project,
    spec,
    total: 0,
    warnings: [] as string[],
    matched: [] as SelectedNode[],
    selectError: "",
    lineage: null as LineageOutput | null,
    lint: null as LintOutput | null,
    lintError: "",
  };
  let engine;
  try {
    engine = await loadEngine({ dir: publicDir("engine") });
  } catch (e) {
    return { ...base, built: false, reason: message(e).split("\n")[0]! };
  }
  let input;
  try {
    input = await sampleInput(project);
    const load = engine.load(input);
    base.total = load.nodes;
    base.warnings = load.warnings;
  } catch (e) {
    return { ...base, built: false, reason: message(e).split("\n")[0]! };
  }
  try {
    const sel = engine.select(spec);
    base.matched = sel.nodes;
    base.selectError = sel.warnings.map((w) => `warning: ${w}`).join("\n");
    base.lineage = engine.lineage({
      roots: base.matched.map((n) => n.unique_id),
      direction: "both",
      depth: 1,
      glyphs: "unicode",
      density: "comfortable",
    });
  } catch (e) {
    base.selectError = `error: ${message(e)} · exit 2`;
  }
  try {
    base.lint = engine.lint({ selector: spec });
  } catch (e) {
    base.lintError = message(e);
  }
  return base;
}
