// The browser side of the lineage renderer: mounts a stage in a host element, redraws it,
// and wires hover, click and double-click. The layout and painting live in lineage.ts.
import type { Graph } from "./graph.ts";
import { layout, paint, type Layout, type LineageNode, type Style } from "./lineage.ts";
import { reveal } from "./motion.ts";

export interface MountOptions {
  onPick?: (id: string) => void;
  onCommit?: (id: string) => void;
}

export interface RenderOptions {
  cursor?: string | null;
  reveal?: { speed?: number; delay?: number } | null;
}

export interface LineageView {
  stage: HTMLElement;
  pre: HTMLElement;
  layout: Layout;
  /** Redraw. The cursor band slides to its new node. Returns the reveal duration in ms. */
  render: (graph: Graph<LineageNode>, styleFor: (L: Layout) => Style, opts?: RenderOptions) => number;
}

/** Mount a renderer in a host element. A stage the server already drew there is reused. */
export function mount(host: HTMLElement, o: MountOptions = {}): LineageView {
  if (!host.querySelector(".lin-stage")) {
    host.innerHTML =
      '<div class="lin-stage"><span class="lin-band" aria-hidden="true"></span><pre class="lin"></pre></div>';
  }
  const stage = host.querySelector<HTMLElement>(".lin-stage")!;
  const band = stage.querySelector<HTMLElement>(".lin-band")!;
  const pre = stage.querySelector<HTMLElement>("pre.lin")!;
  const view = { stage, pre, layout: null as unknown as Layout } as LineageView;

  view.render = (graph, styleFor, opts = {}) => {
    const L = (view.layout = layout(graph));
    pre.innerHTML = paint(L, styleFor(L));
    const r = opts.cursor ? L.rects[opts.cursor] : undefined;
    if (r && opts.cursor) {
      band.style.setProperty("--x", String(r.x + 2));
      band.style.setProperty("--y", String(r.y + 1));
      band.style.setProperty("--w", String(L.by.get(opts.cursor)!.name.length));
    }
    band.classList.toggle("on", !!r);
    if (opts.reveal) return reveal(stage, L, opts.reveal);
    return 0;
  };

  // hover: light a node's own box and the edges that touch it
  const clear = () => pre.querySelectorAll(".hov").forEach((e) => e.classList.remove("hov"));
  pre.addEventListener("pointerover", (ev) => {
    const s = (ev.target as Element).closest("[data-n]");
    clear();
    if (!s) return;
    const q = s.getAttribute("data-n")!.replace(/["\\]/g, "\\$&");
    // data-e holds "|parent>child|" keys; match the id as either end of an edge.
    pre
      .querySelectorAll(`[data-n="${q}"],[data-e*="|${q}>"],[data-e*=">${q}|"]`)
      .forEach((e) => e.classList.add("hov"));
  });
  pre.addEventListener("pointerleave", clear);
  const onNode = (handler: (id: string) => void) => (ev: Event) => {
    const s = (ev.target as Element).closest("[data-n]");
    if (s) handler(s.getAttribute("data-n")!);
  };
  if (o.onPick) pre.addEventListener("click", onNode(o.onPick));
  if (o.onCommit) pre.addEventListener("dblclick", onNode(o.onCommit));
  return view;
}
