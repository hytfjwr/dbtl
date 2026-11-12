// The playground's output as pure functions: for a project and a selector, which nodes match,
// what the lineage draws, and what the list and the lint tab say. The page renders the first
// state at build time and playground.ts keeps it live, both through these functions.
import { index, sub, type Graph } from "./graph.ts";
import { esc } from "./html.ts";
import type { Layout, Style } from "./lineage.ts";
import type { LintData, Project, ProjectNode } from "./samples.ts";
import { evaluate, SelectorError } from "./selector.ts";

/** Selector chips per project. The first one is the starting selector. */
export const EXAMPLES: Record<string, string[]> = {
  sample: [
    "+fct_orders",
    "stg_orders+",
    "meta:owner=alice",
    "config.materialized:table,path:marts",
    "1+cll_*",
    "+exposure:orders_dashboard",
  ],
  jaffle_finance: [
    "stg_payment__*",
    "tag:marts",
    "tag:monthly+",
    "config.materialized:table,path:marts",
    "1+int_suppliers__combined+",
    "+exposure:subscription_churn_notebook",
  ],
};

/** The projects the switcher offers, in order. The first one is open at the start. */
export const PROJECTS = ["jaffle_finance", "sample"];

/** A diagram with more nodes than this is not drawn; the list still shows every match. */
export const DRAW_LIMIT = 40;

export interface Outcome {
  hit: Set<string>;
  /** A message for a selector `dbtl ls` would reject; empty otherwise. */
  error: string;
}

export function run(p: Project, spec: string): Outcome {
  const s = spec.trim();
  if (s === "") return { hit: new Set(), error: "" };
  try {
    return { hit: evaluate(p, s), error: "" };
  } catch (e) {
    if (!(e instanceof SelectorError)) throw e;
    return { hit: new Set(), error: `error: ${e.message} · exit 2` };
  }
}

/** The matched nodes with their direct neighbours as context. */
export function drawn(p: Project, hit: Set<string>): Graph<ProjectNode> {
  const ids = new Set(hit);
  for (const [a, b] of p.edges) {
    if (hit.has(a)) ids.add(b);
    if (hit.has(b)) ids.add(a);
  }
  return sub(p, ids);
}

export function lineageStyle(hit: Set<string>, was: Set<string> | null): (L: Layout) => Style {
  return () => ({
    node: (id) => ({
      hit: hit.has(id),
      dim: !hit.has(id),
      fresh: !!was && hit.has(id) && !was.has(id),
    }),
    edge: (keys) =>
      keys.some((k) => {
        const [a, b] = k.split(">");
        return hit.has(a!) && hit.has(b!);
      })
        ? ""
        : "dim",
  });
}

/** What the lineage pane says when it draws nothing, or null when there is a diagram. */
export function lineageNote(p: Project, o: Outcome, spec: string): string | null {
  if (o.error) return "The selector is not valid.";
  if (spec.trim() === "") return "Type a selector, or pick one above.";
  if (o.hit.size === 0) return "No nodes match.";
  const n = drawn(p, o.hit).nodes.length;
  if (n > DRAW_LIMIT) return `${n} nodes is too many to draw. The list shows every match.`;
  return null;
}

const byName = (p: Project, ids: Iterable<string>): ProjectNode[] => {
  const ix = index(p);
  return [...ids]
    .map((id) => ix.by.get(id)!)
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
};

export function matchedTitle(p: Project, o: Outcome): string {
  return `matched <b>${o.hit.size}</b> of ${p.nodes.length}`;
}

export function matchedHtml(p: Project, o: Outcome, spec: string, was: Set<string> | null): string {
  if (o.hit.size === 0) {
    const text = o.error
      ? "The selector is not valid."
      : spec.trim() === ""
        ? "Type a selector, or pick one above."
        : "No nodes match. An empty match prints nothing and exits 0.";
    return `<li class="none">${text}</li>`;
  }
  return byName(p, o.hit)
    .map(
      (n) =>
        `<li${was && !was.has(n.id) ? ' class="fresh"' : ""}><span>${esc(n.name)}</span>` +
        `<i class="k-${esc(n.tag.toLowerCase())}">${esc(n.tag)}</i></li>`,
    )
    .join("");
}

/** The shell-quoted `dbtl ls` command for a selector. */
export function command(spec: string): string {
  const s = spec.trim() || "<selector>";
  return `dbtl ls '${s.replace(/'/g, "'\\''")}'`;
}

export interface LintView {
  html: string;
  count: number;
}

export function lintHtml(lint: LintData, hit: Set<string>, o: Outcome, spec: string): LintView {
  const s = spec.trim();
  const head = `<div class="cmdline">$ ${esc(`dbtl lint${s ? ` --select '${s.replace(/'/g, "'\\''")}'` : ""}`)}</div>`;
  if (o.error) {
    const message = o.error.replace(/^error: | · exit 2$/g, "");
    return {
      count: 0,
      html:
        head +
        `<div class="f"><span class="fail">ERR</span><span>usage</span><span class="node"></span>` +
        `<span class="msg">${esc(message)}</span></div><div class="sum">no rules evaluated · exit 2</div>`,
    };
  }
  const found = lint.findings.filter((f) => s === "" || hit.has(f.uid));
  const errors = found.filter((f) => f.severity === "error").length;
  const warnings = found.length - errors;
  const rows = found.length
    ? found
        .map(
          (f) =>
            `<div class="f"><span class="${f.severity === "error" ? "fail" : "warn"}">${f.severity === "error" ? "FAIL" : "WARN"}</span>` +
            `<span>${esc(f.rule)}</span><span class="node">${esc(f.file)}</span><span class="msg">${esc(f.message)}</span></div>`,
        )
        .join("")
    : '<div class="f"><span class="pass">PASS</span><span>all rules</span><span class="node"></span>' +
      '<span class="msg">No findings on the selected nodes.</span></div>';
  const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
  return {
    count: found.length,
    html:
      head +
      rows +
      `<div class="sum">${plural(errors, "error")}, ${plural(warnings, "warning")} · ${lint.rules} rules evaluated · exit ${errors ? 1 : 0}</div>`,
  };
}
