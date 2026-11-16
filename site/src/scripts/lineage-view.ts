// The browser side of the lineage pane: mounts a stage in a host element, redraws it,
// and wires hover, click and double-click. The cell grid and painting live in
// engine-lineage.ts — the engine's own layout, not a second renderer.
import { paint, type Lin, type Style } from "./engine-lineage.ts";
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
  layout: Lin;
  /** Redraw. The cursor band slides to its new node. Returns the reveal duration in ms. */
  render: (lin: Lin, styleFor: (L: Lin) => Style, opts?: RenderOptions) => number;
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
  const view = { stage, pre, layout: null as unknown as Lin } as LineageView;

  view.render = (lin, styleFor, opts = {}) => {
    const L = (view.layout = lin);
    pre.innerHTML = paint(L, styleFor(L));
    const r = opts.cursor ? L.rects[opts.cursor] : undefined;
    if (r && opts.cursor) {
      band.style.setProperty("--x", String(r.x + (r.h >= 3 ? 2 : 1)));
      band.style.setProperty("--y", String(r.y + (r.h >= 3 ? 1 : 0)));
      band.style.setProperty("--w", String(L.names.get(opts.cursor)?.length ?? 0));
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
