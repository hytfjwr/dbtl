// The home demo panel as pure functions: what the model list, the lineage and the
// inspector show for a given root and cursor. The page renders the first state at build
// time and home.ts keeps it live in the browser, both through these functions.
import { between, index, sub, walk, type Graph } from "./graph.ts";
import { esc } from "./html.ts";
import type { Layout, Style } from "./lineage.ts";
import { coverage, type Project, type ProjectNode } from "./samples.ts";

/** dbtl's list order: layers in this order, any other layer after them by name. */
const LAYER_ORDER = ["staging", "intermediate", "marts", "utilities"];

const layerRank = (layer: string) => {
  const i = LAYER_ORDER.indexOf(layer);
  return i < 0 ? LAYER_ORDER.length : i;
};

/** The root with its upstream and downstream, each limited to `depth` hops, as the TUI's depth-limited view. */
export function neighbourhood(p: Project, rootId: string, depth: number): Graph<ProjectNode> {
  const ids = walk(p, rootId, "up", depth);
  for (const x of walk(p, rootId, "down", depth)) ids.add(x);
  ids.add(rootId);
  return sub(p, ids);
}

/** Nodes on a path between the root and the cursor. Empty while the cursor is on the root. */
export function pathSet(p: Project, rootId: string, cursor: string): Set<string> {
  return cursor === rootId ? new Set() : between(p, rootId, cursor);
}

/**
 * The lineage style for a root and cursor. `was` is the previous path: nodes that just joined
 * it are marked fresh so the path appears to trace in.
 */
export function demoStyle(
  rootId: string,
  cursor: string,
  path: Set<string>,
  was: Set<string>,
): (L: Layout) => Style {
  const has = path.size > 0;
  return (L) => ({
    originX: L.rects[rootId]?.x ?? 0,
    node: (id) => ({
      root: id === rootId,
      cur: id === cursor && id !== rootId,
      on: has && path.has(id),
      dim: has && !path.has(id),
      fresh: has && path.has(id) && !was.has(id),
    }),
    edge: (keys) => {
      if (!has) return "";
      let on = false;
      let fresh = false;
      for (const k of keys) {
        const [a, b] = k.split(">");
        if (path.has(a!) && path.has(b!)) {
          on = true;
          if (!was.has(a!) || !was.has(b!)) fresh = true;
        }
      }
      return on ? (fresh ? "on fresh" : "on") : "dim";
    },
  });
}

/** Models, grouped by layer, as buttons. */
export function listHtml(p: Project, rootId: string): string {
  const layers = [...new Set(p.nodes.filter((n) => n.type === "model").map((n) => n.layer))].sort(
    (a, b) => layerRank(a) - layerRank(b) || (a < b ? -1 : a > b ? 1 : 0),
  );
  return layers
    .map((layer) => {
      const models = p.nodes
        .filter((n) => n.type === "model" && n.layer === layer)
        .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
      return (
        `<div class="grp">${esc(layer)}</div>` +
        models
          .map(
            (n) =>
              `<button type="button" data-id="${esc(n.id)}" aria-current="${n.id === rootId}">${esc(n.name)}</button>`,
          )
          .join("")
      );
    })
    .join("");
}

export function impact(p: Project, id: string): { up: number; down: number } {
  return { up: walk(p, id, "up").size, down: walk(p, id, "down").size };
}

const MAX_COLUMNS = 8;
const MAX_DESC = 110;

/** The inspector for one node. */
export function inspectorHtml(p: Project, id: string): string {
  const n = index(p).by.get(id)!;
  const { up, down } = impact(p, id);
  const flat = n.desc.replace(/\s+/g, " ").trim();
  const desc = flat.length > MAX_DESC ? `${flat.slice(0, MAX_DESC - 1)}…` : flat;
  const shown = n.cols.slice(0, MAX_COLUMNS);
  const more = n.cols.length - shown.length;
  return (
    `<div class="nm">${esc(n.name)}</div><dl>` +
    `<dt>kind</dt><dd>${esc(n.tag)}</dd>` +
    `<dt>path</dt><dd>${esc(n.path)}</dd>` +
    `<dt>tags</dt><dd>${n.tags.length ? esc(n.tags.join(", ")) : "-"}</dd>` +
    `<dt>tests</dt><dd>${n.tests}</dd>` +
    `<dt>impact</dt><dd>↑${up} upstream · ↓${down} downstream</dd>` +
    `<dt>desc</dt><dd>${desc ? esc(desc) : '<span class="fail">missing</span>'}</dd></dl>` +
    (n.cols.length
      ? `<ul class="cols">${shown.map((c) => `<li><span>${esc(c[0])}</span><i>${esc(c[1])}</i></li>`).join("")}` +
        (more > 0 ? `<li><span>+${more} more</span></li>` : "") +
        "</ul>"
      : "")
  );
}

/** The pane title. The tag is the TUI's view label: both directions, limited to `depth` hops. */
export const titleHtml = (rootName: string, depth: number): string =>
  `Lineage: <b>${esc(rootName)}</b> [↑↓ ≤${depth}]`;

export const impactText = (p: Project, id: string): string => {
  const { up, down } = impact(p, id);
  return `↑${up} ↓${down}`;
};

export const coverageText = (p: Project): string => `cov ${coverage(p)}%`;

/**
 * What the demo shows. `root` is the model the lineage opens on; `trail` is the cursor's walk away
 * from it. The page opens with the cursor at the end of the trail, and the first-visit animation
 * replays the walk from the root.
 */
export const HOME = {
  project: "jaffle_finance",
  root: "fct_subscription_process",
  /** Hops kept on each side of the root, and kept when re-rooting. */
  depth: 1,
  trail: [
    "int_subscriptions__combined",
    "fct_delivery_monthly_snapshot",
    "subscription_churn_notebook",
  ],
};

/** Resolve the home config to node ids. Throws when the data no longer has these names. */
export function homeIds(p: Project): { root: string; trail: string[] } {
  const byName = new Map(p.nodes.map((n) => [n.name, n.id] as const));
  const id = (name: string) => {
    const found = byName.get(name);
    if (!found) throw new Error(`home demo: no node named ${name} in ${p.name}`);
    return found;
  };
  return { root: id(HOME.root), trail: HOME.trail.map(id) };
}
