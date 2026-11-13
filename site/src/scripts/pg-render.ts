// The playground's output as pure functions over engine results: matched list,
// lineage grid, lint report and column lineage all become the same markup at
// build time (SSR) and in the browser, so the static page and the live DOM can
// never diverge. No DOM, no engine handle — just data in, HTML strings out.
import type { ColumnsOutput, LineageOutput, LintOutput, SelectedNode } from "../engine/index.ts";
import { SAMPLE_NAMES } from "../../scripts/samples.ts";
import { esc } from "./html.ts";
import { tagOf } from "./samples.ts";

/** The bundled projects the switcher offers, in order. The first is open at start. */
export const SAMPLES = SAMPLE_NAMES;
export type SampleName = (typeof SAMPLES)[number];

/** Selector chips per project. The first one is the starting selector. */
export const EXAMPLES: Record<string, string[]> = {
  jaffle_finance: [
    "stg_payment__*",
    "tag:marts",
    "tag:monthly+",
    "config.materialized:table,path:models/marts",
    "1+int_suppliers__combined+",
    "+exposure:subscription_churn_notebook",
  ],
  sample: [
    "+fct_orders",
    "stg_orders+",
    "meta:owner=alice",
    "config.materialized:table,path:models/marts",
    "1+cll_*",
    "+exposure:orders_dashboard",
  ],
};

/** The chips an uploaded manifest gets — only examples that match every project. */
export const GENERIC_EXAMPLES = ["*"];

/** The .dbtl.yml the edit pane starts from — comments only, i.e. the defaults. */
export const CONFIG_TEMPLATE = `# .dbtl.yml — project configuration for this run.
# Empty file (or comments only) = shipped defaults. Example:
# lint:
#   rules:
#     select_star: {severity: warn}
# fail_on: error
`;

const squote = (s: string): string => `'${s.replace(/'/g, "'\\''")}'`;

/** The shell-quoted `dbtl ls` command for a selector. */
export const lsCommand = (spec: string): string => `dbtl ls ${squote(spec.trim() || "<selector>")}`;

/** The `dbtl lint` command for the selector currently narrowing the findings. */
export const lintCommand = (spec: string): string =>
  `dbtl lint${spec.trim() ? ` --select ${squote(spec.trim())}` : ""}`;

/** The `dbtl columns` command for a node.column target. */
export const columnsCommand = (target: string, upstream: boolean): string =>
  `dbtl columns ${target || "<model>.<column>"}${upstream ? " --upstream" : ""}`;

/* --- matched list -------------------------------------------------------- */

const byName = (a: SelectedNode, b: SelectedNode) =>
  a.name < b.name ? -1 : a.name > b.name ? 1 : a.unique_id < b.unique_id ? -1 : 1;

export const matchedTitle = (total: number, count: number): string =>
  `matched <b>${count}</b> of ${total}`;

/**
 * The matched-node list. `was` marks rows that just joined the match (the
 * fresh flash); `picked` marks the node whose detail is open.
 */
export function matchedHtml(
  nodes: SelectedNode[],
  opts: {
    error?: string;
    spec: string;
    was?: Set<string> | null;
    picked?: string | null;
  },
): string {
  if (nodes.length === 0) {
    const text = opts.error
      ? "The selector is not valid."
      : opts.spec.trim() === ""
        ? "Type a selector, or pick one above."
        : "No nodes match. An empty match prints nothing and exits 0.";
    return `<li class="none">${text}</li>`;
  }
  const was = opts.was ?? null;
  return [...nodes]
    .sort(byName)
    .map((n) => {
      const tag = tagOf(n.resource_type, n.materialized);
      const cl =
        `${was && !was.has(n.unique_id) ? "fresh" : ""}${n.unique_id === opts.picked ? " cur" : ""}`.trim();
      return (
        `<li${cl ? ` class="${cl}"` : ""} data-id="${esc(n.unique_id)}"><span>${esc(n.name)}</span>` +
        `<i class="k-${tag.toLowerCase()}">${esc(tag)}</i></li>`
      );
    })
    .join("");
}

/* --- lineage grid -------------------------------------------------------- */

// Engine class codes → the k-* color classes the site already uses.
const CLASS: Record<string, string> = {
  t: "k-table",
  v: "k-view",
  i: "k-incremental",
  e: "k-ephemeral",
  m: "k-model",
  s: "k-source",
  d: "k-seed",
  n: "k-snapshot",
  x: "k-exposure",
  c: "ts",
};

// The cells a node's label occupies — the same geometry the engine's box
// drawing uses: bordered boxes put the label on row y+1 starting at x+2,
// clipped to the interior; one-row boxes put it on y starting at x+1.
function nameCells(r: { x: number; y: number; w: number; h: number }, name: string): Set<number> {
  const cells = new Set<number>();
  const start = r.h >= 3 ? 2 : 1;
  let i = 0;
  for (const _ of name) {
    if (start + i >= r.w - 1) break;
    cells.add((r.y + (r.h >= 3 ? 1 : 0)) * 1_000_000 + r.x + start + i);
    i += 1;
  }
  return cells;
}

/**
 * The engine's character grid painted with the site's lineage classes: matched
 * nodes keep their materialization colour with the band on their names,
 * context nodes dim, and `picked` carries the cursor band (`cur`).
 */
export function lineageHtml(
  out: LineageOutput,
  opts: {
    /** uid → name, for every matched node. */
    hit: Map<string, string>;
    /** The node whose detail is open — gets the cursor ink. */
    picked?: { uid: string; name: string } | null;
    /** uids matched for the first time — staggered flash. */
    fresh?: Set<string> | null;
    originX?: number;
  },
): string {
  // Per-cell "name" flags: the label cells of nodes we want to call out —
  // every match, plus the picked node (which may sit in the dim context).
  const named = new Map<number, string>();
  for (const r of out.rects) {
    const hitName = opts.hit.get(r.uid);
    const pickedName = opts.picked?.uid === r.uid ? opts.picked.name : undefined;
    const name = pickedName ?? hitName;
    if (name === undefined) continue;
    for (const c of nameCells(r, name)) named.set(c, r.uid);
  }
  const rectOf = new Map<number, string>();
  for (const r of out.rects) {
    for (let y = r.y; y < r.y + r.h; y++) {
      for (let x = r.x; x < r.x + r.w; x++) rectOf.set(y * 1_000_000 + x, r.uid);
    }
  }

  return out.rows
    .map((row, y) => {
      let html = "";
      let key: string | null = null;
      let attrs = "";
      let buf = "";
      const flush = () => {
        if (buf) {
          html += attrs ? `<span${attrs}>${buf}</span>` : buf;
          buf = "";
        }
      };
      for (let x = 0; x < row.length; x++) {
        const ch = row[x]!;
        const cell = y * 1_000_000 + x;
        const uid = named.get(cell);
        const owner = rectOf.get(cell);
        let a = "";
        if (ch !== " ") {
          let cl = "";
          if (uid !== undefined) {
            const cur = opts.picked?.uid === uid;
            cl =
              "nm" +
              (cur ? " cur" : ` hit`) +
              (out.emphasis[y]?.[x] === "1" ? " root" : "") +
              (opts.fresh?.has(uid) ? " fresh" : "");
          } else if (owner !== undefined) {
            cl = `b ${opts.hit.has(owner) ? (CLASS[out.classes[y]?.[x] ?? " "] ?? "b") : "dim"}`;
          } else {
            cl = "e";
          }
          const extra = owner !== undefined ? ` data-n="${esc(owner)}"` : "";
          a = (cl ? ` class="${cl}"` : "") + extra;
        }
        if (a !== key) {
          flush();
          key = a;
          attrs =
            a +
            (a.includes("fresh")
              ? ` style="--i:${Math.round(Math.abs(x - (opts.originX ?? 0)) / 2)}"`
              : "");
        }
        buf += esc(ch);
      }
      flush();
      return html.replace(/ +$/, "");
    })
    .join("\n");
}

/** The stage markup with the cursor band over `picked`'s name, like the demo's. */
export function stageHtml(out: LineageOutput, opts: Parameters<typeof lineageHtml>[1]): string {
  const rect = opts.picked ? out.rects.find((r) => r.uid === opts.picked!.uid) : undefined;
  const band = rect
    ? ` style="--x:${rect.x + (rect.h >= 3 ? 2 : 1)};--y:${rect.y + (rect.h >= 3 ? 1 : 0)};--w:${opts.picked!.name.length}"`
    : "";
  return (
    `<div class="lin-stage"><span class="lin-band${rect ? " on" : ""}"${band} aria-hidden="true"></span>` +
    `<pre class="lin">${lineageHtml(out, opts)}</pre></div>`
  );
}

/* --- ANSI → HTML --------------------------------------------------------- */

// SGR parameter → the site's colour/weight token classes. The CLI's palette is
// small: bold, the severity colours and the bright help/link accents.
const SGR: Record<number, string> = {
  1: "a-b",
  2: "a-dim",
  3: "a-i",
  4: "a-u",
  31: "a-red",
  32: "a-green",
  33: "a-yellow",
  34: "a-blue",
  35: "a-magenta",
  36: "a-cyan",
  90: "a-dim",
  91: "a-red",
  92: "a-green",
  93: "a-yellow",
  94: "a-blue",
  95: "a-magenta",
  96: "a-cyan",
};

/**
 * Bare http(s) URLs become real links, trailing sentence punctuation
 * excluded. Takes raw text and escapes each piece itself — escaping first
 * would let an entity's `;` end up in the trimmed tail and corrupt the href.
 */
const linkify = (text: string): string =>
  text.replace(/https?:\/\/[^\s"'<>()]+/g, (m) => {
    const tail = /[.,;:]*$/.exec(m)![0];
    const url = m.slice(0, m.length - tail.length);
    return `<a href="${esc(url)}" target="_blank" rel="noopener">${esc(url)}</a>${esc(tail)}`;
  });

// SGR codes that undo a style without touching the rest.
const SGR_UNDO: Record<number, string[]> = {
  22: ["a-b", "a-dim"],
  23: ["a-i"],
  24: ["a-u"],
  39: ["a-red", "a-green", "a-yellow", "a-blue", "a-magenta", "a-cyan"],
  49: ["a-red", "a-green", "a-yellow", "a-blue", "a-magenta", "a-cyan"],
};

/**
 * The one ANSI→HTML conversion: SGR codes become token-class spans, everything
 * else is escaped and URL-linked. The textContent of the result equals the
 * report's `--format text` byte for byte, so a test can diff them directly.
 */
export function ansiToHtml(ansi: string): string {
  let out = "";
  let classes: string[] = [];
  let open = false;
  let pos = 0;
  // eslint-disable-next-line no-control-regex -- ANSI SGR sequences start with ESC by definition.
  const re = /\x1b\[([0-9;]*)m/g;
  // Text goes out under the current classes — a span is only opened when
  // styled text actually arrives, so back-to-back SGR codes never leave
  // empty spans.
  const text = (t: string) => {
    if (!t) return;
    if (classes.length && !open) {
      out += `<span class="${classes.join(" ")}">`;
      open = true;
    }
    out += linkify(t);
  };
  const restyle = (next: string[]) => {
    classes = next;
    if (open && !next.length) {
      out += "</span>";
      open = false;
    } else if (open) {
      out += `</span><span class="${next.join(" ")}">`;
    }
  };
  for (let m = re.exec(ansi); m; m = re.exec(ansi)) {
    text(ansi.slice(pos, m.index));
    let next = classes;
    for (const p of m[1]!.split(";")) {
      const code = p === "" ? 0 : Number(p);
      if (code === 0) next = [];
      else if (SGR_UNDO[code]) next = next.filter((c) => !SGR_UNDO[code]!.includes(c));
      else {
        const cl = SGR[code];
        if (cl && !next.includes(cl)) next = [...next, cl];
      }
    }
    if (next.join(" ") !== classes.join(" ")) restyle(next);
    else if (next !== classes) classes = next;
    pos = re.lastIndex;
  }
  text(ansi.slice(pos));
  if (open) out += "</span>";
  return out;
}

/* --- lint report --------------------------------------------------------- */

/** The finding count for the tab badge, from the report's own summary object. */
export function lintCount(lint: LintOutput | null): number {
  if (!lint) return 0;
  try {
    const { summary } = JSON.parse(lint.json) as {
      summary?: { errors?: number; warnings?: number };
    };
    return (summary?.errors ?? 0) + (summary?.warnings ?? 0);
  } catch {
    return 0;
  }
}

/**
 * The lint pane's body: the engine's ANSI report converted in place, or the
 * thrown error the CLI would have printed on stderr (`dbtl lint` exits 2 on a
 * bad selector or a broken config).
 */
export function lintBodyHtml(lint: LintOutput | null, lintError: string): string {
  if (lintError) return `<pre class="pg-cli">error: ${esc(lintError)} · exit 2</pre>`;
  if (!lint) return `<pre class="pg-cli"></pre>`;
  return `<pre class="pg-cli">${ansiToHtml(lint.ansi)}</pre>`;
}

/**
 * What the pane shows under the report: engine warnings and the gate error are
 * the CLI's stderr lines; the exit code is the process's. `noExcerpts` is the
 * uploaded-manifest caveat — without the project's .yml files, findings can
 * cite positions but cannot quote them.
 */
export function lintFootHtml(
  lint: LintOutput | null,
  opts: { noExcerpts?: boolean; extra?: string[] } = {},
): string {
  const lines: string[] = [];
  for (const w of opts.extra ?? []) lines.push(`<p class="w">warning: ${esc(w)}</p>`);
  if (lint) {
    for (const w of lint.warnings) lines.push(`<p class="w">warning: ${esc(w)}</p>`);
    if (lint.error) lines.push(`<p class="e">error: ${esc(lint.error)}</p>`);
    lines.push(`<p class="x">exit ${lint.exit_code}</p>`);
  }
  if (opts.noExcerpts) {
    lines.push(
      `<p class="n">excerpts cite positions only — this manifest came without its .yml files</p>`,
    );
  }
  return lines.join("");
}

/* --- columns ------------------------------------------------------------- */

/** The columns pane's empty state — one string for SSR and the client. */
export const COLS_EMPTY_HINT =
  "Pick a node and a column — or pick a node in the Select tab and choose “columns”.";

/**
 * The columns pane's body: `dbtl columns`' own Markdown table, verbatim.
 * `hint` is the empty-state line (no target picked yet / no columns on it).
 */
export function columnsBodyHtml(out: ColumnsOutput | null, hint: string): string {
  if (!out) return `<pre class="pg-cli">${esc(hint)}</pre>`;
  return `<pre class="pg-cli">${linkify(out.markdown)}</pre>`;
}

/** `<option>`s for the column-tab node picker. */
export function nodeOptionsHtml(nodes: SelectedNode[], picked: string | null): string {
  return [...nodes]
    .sort(byName)
    .map(
      (n) =>
        `<option value="${esc(n.unique_id)}"${n.unique_id === picked ? " selected" : ""}>` +
        `${esc(n.name)} · ${esc(tagOf(n.resource_type, n.materialized))}</option>`,
    )
    .join("");
}

/** `<option>`s for the column picker itself. */
export function columnOptionsHtml(columns: string[], picked: string | null): string {
  return columns
    .map((c) => `<option value="${esc(c)}"${c === picked ? " selected" : ""}>${esc(c)}</option>`)
    .join("");
}
