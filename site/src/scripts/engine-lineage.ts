// The home demo's lineage pane driven by the engine's own layout: a
// LineageOutput-shaped payload becomes the cell grid the painter reads —
// chars, cell kinds, edge attribution and box classes all come from the
// engine, so the demo draws exactly what the TUI places and there is no
// second layout algorithm to drift.
import type { LineageCellEdges, LineageEdge, LineageRect } from "../engine/index.ts";
import { esc } from "./html.ts";

/** The engine lineage fields the painter reads (the prebuilt data file's per-root record). */
export interface LineageData {
  w: number;
  h: number;
  rows: string[];
  kinds: string[];
  classes: string[];
  rects: LineageRect[];
  edges: LineageEdge[];
  cell_edges: LineageCellEdges[];
}

export interface Cell {
  ch: string;
  /** e: connector, a: arrowhead, b: box border, p: box padding, n: name, x: tests label */
  k: "e" | "a" | "b" | "p" | "n" | "x";
  /** Materialization class on box-border cells: "k-table", "k-view", ... */
  cls?: string;
  es?: string[];
  n?: string;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
  col: number;
}

/** The paintable view over an engine layout: cells plus the geometry the cursor and reveal need. */
export interface Lin {
  W: number;
  H: number;
  cells: (Cell | null)[][];
  rects: Record<string, Rect>;
  /** Node ids per column, in stacking (top-down) order. */
  columns: string[][];
  colX: number[];
  colW: number[];
  /** uid → display name, for the cursor band width. */
  names: Map<string, string>;
}

export interface NodeStyle {
  dim?: boolean;
  on?: boolean;
  root?: boolean;
  cur?: boolean;
  hit?: boolean;
  fresh?: boolean;
}

export interface Style {
  node: (id: string) => NodeStyle;
  /** Edge keys are "parent>child". Returns "", "on", "on fresh" or "dim". */
  edge: (keys: string[]) => string;
  originX?: number;
}

// Engine class codes → the k-* color classes the site uses (same map as the playground).
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

/**
 * Turn the engine's lineage output into the paintable cell grid. Rects supply
 * the box-owner attribution and the column geometry (columns are rect x-groups
 * stacked by y — the layout's own stacking order); kinds supply the cell kind;
 * cell_edges supplies every edge routed through each connector cell.
 */
export function cells(data: LineageData, names: Map<string, string> = new Map()): Lin {
  const { w: W, h: H } = data;
  const at = (x: number, y: number) => y * W + x;

  const es = new Map<number, string[]>();
  for (const ce of data.cell_edges) {
    es.set(
      at(ce.x, ce.y),
      ce.edges.map((i) => `${data.edges[i]!.parent}>${data.edges[i]!.child}`),
    );
  }

  const rects: Record<string, Rect> = {};
  const byX = new Map<number, LineageRect[]>();
  for (const r of data.rects) {
    const group = byX.get(r.x) ?? [];
    group.push(r);
    byX.set(r.x, group);
  }
  const colX = [...byX.keys()].sort((a, b) => a - b);
  const columns: string[][] = [];
  const colW: number[] = [];
  colX.forEach((x, c) => {
    const group = byX
      .get(x)!
      .slice()
      .sort((a, b) => a.y - b.y);
    columns.push(group.map((r) => r.uid));
    colW.push(Math.max(...group.map((r) => r.x + r.w)) - x);
    for (const r of group) rects[r.uid] = { x: r.x, y: r.y, w: r.w, h: r.h, col: c };
  });

  const owner = new Map<number, string>();
  for (const r of data.rects) {
    for (let y = r.y; y < r.y + r.h; y++) {
      for (let x = r.x; x < r.x + r.w; x++) owner.set(at(x, y), r.uid);
    }
  }

  const grid: (Cell | null)[][] = [];
  for (let y = 0; y < H; y++) {
    const row: (Cell | null)[] = [];
    // The engine's grids count cells; [...s] keeps indexing aligned when a
    // label carries a non-BMP glyph (kinds/classes are ASCII by contract).
    const chars = [...(data.rows[y] ?? "")];
    const kinds = data.kinds[y] ?? "";
    const classes = data.classes[y] ?? "";
    for (let x = 0; x < W; x++) {
      const k = kinds[x];
      if (k === undefined || k === " ") {
        row.push(null);
        continue;
      }
      const c: Cell = { ch: chars[x] ?? " ", k: k as Cell["k"] };
      const keys = es.get(at(x, y));
      if (keys) c.es = keys;
      if (k === "b") c.cls = CLASS[classes[x] ?? " "] ?? "b";
      const n = owner.get(at(x, y));
      if (n !== undefined && k !== "e" && k !== "a") c.n = n;
      row.push(c);
    }
    grid.push(row);
  }
  return { W, H, cells: grid, rects, columns, colX, colW, names };
}

/** The layout as HTML: runs of cells with one class set become one span. */
export function paint(L: Lin, style: Style): string {
  const ox = style.originX ?? 0;
  return L.cells
    .map((row) => {
      let out = "";
      let key: string | null = null;
      let attrs = "";
      let buf = "";
      const flush = () => {
        if (buf) {
          out += attrs ? `<span${attrs}>${buf}</span>` : buf;
          buf = "";
        }
      };
      row.forEach((c, x) => {
        let a = "";
        if (c) {
          let cl = "";
          let extra = "";
          if (c.k === "e" || c.k === "a") {
            const s = style.edge(c.es!);
            cl = `e${s ? ` ${s}` : ""}`;
            extra = ` data-e="|${esc(c.es!.join("|"))}|"`;
          } else {
            const s = style.node(c.n!);
            if (c.k === "b") {
              cl = `b ${s.dim ? "dim" : s.on || s.cur ? "on" : (c.cls ?? "b")}${s.fresh ? " fresh" : ""}`;
            } else if (c.k === "x") {
              cl = s.dim ? "ts dim" : "ts";
            } else if (c.k === "n") {
              cl =
                "nm" +
                (s.dim ? " dim" : "") +
                (s.root ? " root" : "") +
                (s.cur ? " cur" : "") +
                (s.hit ? " hit" : "") +
                (s.fresh ? " fresh" : "");
            }
            extra = ` data-n="${esc(c.n!)}"`;
          }
          a = (cl ? ` class="${cl}"` : "") + extra;
        }
        if (a !== key) {
          flush();
          key = a;
          attrs =
            a + (a.includes("fresh") ? ` style="--i:${Math.round(Math.abs(x - ox) / 2)}"` : "");
        }
        buf += c ? esc(c.ch) : " ";
      });
      flush();
      return out.replace(/ +$/, "");
    })
    .join("\n");
}

/** The stage markup `mount` adopts: the cursor band and the painted `<pre>`. */
export function stageHtml(L: Lin, style: Style, cursor?: string | null): string {
  const r = cursor ? L.rects[cursor] : undefined;
  const band = r
    ? ` style="--x:${r.x + (r.h >= 3 ? 2 : 1)};--y:${r.y + (r.h >= 3 ? 1 : 0)};--w:${L.names.get(cursor!)?.length ?? 0}"`
    : "";
  return (
    `<div class="lin-stage"><span class="lin-band${r ? " on" : ""}"${band} aria-hidden="true"></span>` +
    `<pre class="lin">${paint(L, style)}</pre></div>`
  );
}
