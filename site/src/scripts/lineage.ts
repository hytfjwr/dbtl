// Text lineage renderer. Input: {nodes: [{id, name, tag, tests}], edges: [[parent, child]]}.
// The layout follows dbtl's own rules:
// - column = longest path from a root, so every edge goes strictly right
// - a column stacks its nodes by id (unique_id), top down, one blank row between boxes
// - column width = its widest box; a 3-cell gutter between columns
// - box: top border carries the tag, middle row the name, bottom border the tests label, right-aligned
// - an edge leaves the parent's middle row. Same row: a straight run. Otherwise it turns in a
//   channel two cells left of the child and enters through the arrowhead cell
// - edges are drawn in (parent, child) order and the first glyph in a cell wins; then arrowheads;
//   then boxes on top. A run that spans several columns passes behind the boxes in between and
//   shows in the gutters
import type { Graph, GraphNode } from "./graph.ts";
import { esc } from "./html.ts";

export interface LineageNode extends GraphNode {
  /** Materialization tag drawn in the top border: View, Table, Source, ... */
  tag: string;
  tests: number;
}

export interface Cell {
  ch: string;
  /** e: connector, a: arrowhead, b: box border, p: box padding, n: name, x: tests label */
  k: "e" | "a" | "b" | "p" | "n" | "x";
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

export interface Layout {
  W: number;
  H: number;
  cells: (Cell | null)[][];
  rects: Record<string, Rect>;
  columns: string[][];
  colX: number[];
  colW: number[];
  kind: Record<string, string>;
  by: Map<string, LineageNode>;
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

const GUTTER = 3;
const BOX_H = 3;
const ROW_GAP = 1;
const G = { h: "─", v: "│", tl: "╭", tr: "╮", bl: "╰", br: "╯", arrow: "▶" };

const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
const testsLabel = (n: LineageNode) => (n.tests > 0 ? `tests:${n.tests}` : "");
const boxWidth = (n: LineageNode) => Math.max(n.name.length + 2, n.tag.length + 1, testsLabel(n).length + 1) + 2;

export function layout(graph: Graph<LineageNode>): Layout {
  const nodes = graph.nodes.slice().sort((a, b) => cmp(a.id, b.id));
  const by = new Map(nodes.map((n) => [n.id, n] as const));
  const edges = graph.edges
    .filter(([p, c]) => by.has(p) && by.has(c))
    .sort((a, b) => cmp(a[0], b[0]) || cmp(a[1], b[1]));
  const parents = new Map<string, string[]>(nodes.map((n) => [n.id, []]));
  for (const [p, c] of edges) parents.get(c)!.push(p);

  const col = new Map<string, number>();
  const colOf = (id: string): number => {
    const known = col.get(id);
    if (known !== undefined) return known;
    col.set(id, 0);
    let c = 0;
    for (const p of parents.get(id)!) c = Math.max(c, colOf(p) + 1);
    col.set(id, c);
    return c;
  };
  for (const n of nodes) colOf(n.id);

  const ncol = nodes.length ? Math.max(...col.values()) + 1 : 0;
  const columns: string[][] = Array.from({ length: ncol }, () => []);
  for (const n of nodes) columns[col.get(n.id)!].push(n.id);
  const colW = columns.map((ids) => Math.max(1, ...ids.map((id) => boxWidth(by.get(id)!))));
  const colX: number[] = [];
  let x = 0;
  colW.forEach((w, i) => {
    colX.push(x);
    x += w;
    if (i + 1 < colW.length) x += GUTTER;
  });
  const W = Math.max(1, x);
  const rects: Record<string, Rect> = {};
  let H = 1;
  columns.forEach((ids, c) => {
    let y = 0;
    for (const id of ids) {
      rects[id] = { x: colX[c], y, w: boxWidth(by.get(id)!), h: BOX_H, col: c };
      H = Math.max(H, y + BOX_H);
      y += BOX_H + ROW_GAP;
    }
  });
  const cells: (Cell | null)[][] = Array.from({ length: H }, () => new Array<Cell | null>(W).fill(null));

  // pass 1: connector runs and corners
  for (const [pid, cid] of edges) {
    const p = rects[pid];
    const c = rects[cid];
    const key = `${pid}>${cid}`;
    const py = p.y + 1;
    const cy = c.y + 1;
    const fromX = p.x + p.w;
    if (c.x === 0) continue;
    const arrowX = c.x - 1;
    if (fromX > arrowX) continue;
    const draw = (cx: number, row: number, ch: string) => {
      if (!cells[row][cx]) cells[row][cx] = { ch, k: "e", es: [key] };
    };
    const claim = (cx: number, row: number) => {
      const cell = cells[row][cx];
      if (cell && cell.k === "e" && !cell.es!.includes(key)) cell.es!.push(key);
    };
    if (py === cy) {
      for (let hx = fromX; hx < arrowX; hx++) draw(hx, py, G.h);
      continue;
    }
    const channelX = arrowX > fromX ? arrowX - 1 : arrowX;
    for (let hx = fromX; hx < channelX; hx++) draw(hx, py, G.h);
    for (let vy = Math.min(py, cy); vy <= Math.max(py, cy); vy++) {
      const ch = vy === py ? (cy > py ? G.tr : G.br) : vy === cy ? (cy > py ? G.bl : G.tl) : G.v;
      draw(channelX, vy, ch);
    }
    for (let hx = channelX + 1; hx < arrowX; hx++) draw(hx, cy, G.h);
    // the child-side corner is shared by every edge into this child
    for (let sx = channelX; sx < arrowX; sx++) claim(sx, cy);
  }

  // pass 2: arrowheads win their cell
  for (const [pid, cid] of edges) {
    const c = rects[cid];
    const key = `${pid}>${cid}`;
    if (c.x === 0) continue;
    const cell = cells[c.y + 1][c.x - 1];
    if (cell && cell.k === "a") cell.es!.push(key);
    else cells[c.y + 1][c.x - 1] = { ch: G.arrow, k: "a", es: [key] };
  }

  // boxes on top
  for (const n of nodes) {
    const r = rects[n.id];
    const inner = r.w - 2;
    const put = (cx: number, cy: number, ch: string, k: Cell["k"]) => {
      cells[cy][cx] = { ch, k, n: n.id };
    };
    put(r.x, r.y, G.tl, "b");
    put(r.x + r.w - 1, r.y, G.tr, "b");
    for (let i = 0; i < inner; i++) put(r.x + 1 + i, r.y, i < n.tag.length ? n.tag[i] : G.h, "b");
    put(r.x, r.y + 1, G.v, "b");
    put(r.x + r.w - 1, r.y + 1, G.v, "b");
    for (let i = 0; i < inner; i++) put(r.x + 1 + i, r.y + 1, " ", "p");
    for (let i = 0; i < n.name.length; i++) if (1 + i < inner) put(r.x + 2 + i, r.y + 1, n.name[i], "n");
    put(r.x, r.y + 2, G.bl, "b");
    put(r.x + r.w - 1, r.y + 2, G.br, "b");
    for (let i = 0; i < inner; i++) put(r.x + 1 + i, r.y + 2, G.h, "b");
    const t = testsLabel(n);
    const start = r.x + r.w - 2 - t.length;
    for (let i = 0; i < t.length; i++) if (start + i > r.x) put(start + i, r.y + 2, t[i], "x");
  }

  const kind: Record<string, string> = {};
  for (const n of nodes) kind[n.id] = n.tag.toLowerCase();
  return { W, H, cells, rects, columns, colX, colW, kind, by };
}

/** Plain text, trailing spaces kept (the grid is rectangular). */
export const toText = (L: Layout): string =>
  L.cells.map((row) => row.map((c) => (c ? c.ch : " ")).join("")).join("\n");

/** The layout as HTML: runs of cells with one class set become one span. */
export function paint(L: Layout, style: Style): string {
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
              cl = `b ${s.dim ? "dim" : s.on || s.cur ? "on" : `k-${L.kind[c.n!]}`}${s.fresh ? " fresh" : ""}`;
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
          attrs = a + (a.includes("fresh") ? ` style="--i:${Math.round(Math.abs(x - ox) / 2)}"` : "");
        }
        buf += c ? esc(c.ch) : " ";
      });
      flush();
      return out.replace(/ +$/, "");
    })
    .join("\n");
}

/** The stage markup `mount` adopts: the cursor band and the painted `<pre>`. */
export function stageHtml(L: Layout, style: Style, cursor?: string | null): string {
  const r = cursor ? L.rects[cursor] : undefined;
  const band = r
    ? ` style="--x:${r.x + 2};--y:${r.y + 1};--w:${L.by.get(cursor!)!.name.length}"`
    : "";
  return (
    `<div class="lin-stage"><span class="lin-band${r ? " on" : ""}"${band} aria-hidden="true"></span>` +
    `<pre class="lin">${paint(L, style)}</pre></div>`
  );
}

export { boxWidth };
