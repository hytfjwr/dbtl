// The first-visit moment on the home page: the panes draw, the lineage renders column by
// column, then the path traces out from the root. It plays once per browser session, never
// under reduced motion, and never after the visitor has touched the demo.
import { ms, reduced } from "./motion.ts";

const KEY = "dbtl-booted";

/** True when the boot should play now. Records that it did, so a reload in the same session skips it. */
export function claimBoot(): boolean {
  if (reduced()) return false;
  try {
    if (sessionStorage.getItem(KEY)) return false;
    sessionStorage.setItem(KEY, "1");
  } catch {
    // Storage is blocked: playing every time beats never playing.
  }
  return true;
}

export interface BootOptions {
  /** The panel the `booting` class goes on. */
  panel: HTMLElement;
  /** Cursor back on the root and the lineage redrawn with a reveal that starts after `delay` ms. Returns the reveal's length. */
  start: (delay: number) => number;
  /** Nodes the cursor visits after the reveal, in order. */
  trail: string[];
  /** Move the cursor to a node. */
  visit: (id: string) => void;
  /** True once the visitor has interacted; the rest of the animation is dropped. */
  touched: () => boolean;
}

/** Start the boot when the panel is on screen. */
export function playWhenVisible(o: BootOptions): void {
  const run = () => {
    if (o.touched() || !claimBoot()) return;
    const { panel } = o;
    panel.classList.add("booting");
    setTimeout(() => panel.classList.remove("booting"), ms("--dur-3") * 3);
    const total = o.start(ms("--dur-3"));
    o.trail.forEach((id, i) =>
      setTimeout(() => {
        if (!o.touched()) o.visit(id);
      }, total + 120 + i * 300),
    );
  };
  if (reduced()) return;
  if (!("IntersectionObserver" in window)) {
    run();
    return;
  }
  const watcher = new IntersectionObserver(
    (entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      watcher.disconnect();
      run();
    },
    { threshold: 0.1 },
  );
  watcher.observe(o.panel);
}
