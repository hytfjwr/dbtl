/*
 * Motion helpers. Durations live in CSS custom properties (see styles/tokens.css);
 * under reduced motion every helper here applies the final state at once.
 */

const root = document.documentElement;

let query: MediaQueryList | null = null;
try {
  query = window.matchMedia("(prefers-reduced-motion: reduce)");
} catch {
  // no matchMedia: treat as full motion
}

export const reduced = (): boolean => query?.matches === true;

/** A time token such as `--cell`, in milliseconds. */
export function ms(name: string): number {
  const value = getComputedStyle(root).getPropertyValue(name).trim();
  const n = parseFloat(value);
  if (Number.isNaN(n)) return 0;
  return /ms$/.test(value) ? n : n * 1000;
}

export type TransitionKind = "route" | "theme";

/** Run a DOM update inside a view transition. Without support, or under reduced motion, it is a plain call. */
export function transition(kind: TransitionKind, update: () => void): Promise<void> {
  if (reduced() || !document.startViewTransition) {
    update();
    return Promise.resolve();
  }
  root.dataset.vt = kind;
  const done = () => {
    if (root.dataset.vt === kind) delete root.dataset.vt;
  };
  try {
    return document.startViewTransition(update).finished.then(done, done);
  } catch {
    done();
    update();
    return Promise.resolve();
  }
}

/** Column geometry of a text diagram, in character cells. */
export interface RevealGeometry {
  colX: number[];
  colW: number[];
}

/**
 * Reveal a text diagram like a terminal repaint: each column of boxes lands in
 * one step, then its connectors trace through the gutter one cell at a time.
 * Returns the total time in ms (0 when nothing was animated).
 */
export function reveal(
  stage: HTMLElement,
  geometry: RevealGeometry,
  options: { speed?: number; delay?: number } = {},
): number {
  if (reduced() || typeof stage.animate !== "function" || geometry.colX.length === 0) return 0;
  const cell = ms("--cell") * (options.speed ?? 1);
  const delay = options.delay ?? 0;
  const edges = [0];
  const widths = [0];
  geometry.colX.forEach((x, i) => {
    edges.push(x + geometry.colW[i]!);
    widths.push(5);
    if (i < geometry.colX.length - 1) {
      for (let g = 1; g <= 3; g++) {
        edges.push(x + geometry.colW[i]! + g);
        widths.push(1);
      }
    }
  });
  const total = widths.reduce((a, b) => a + b, 0);
  let acc = 0;
  const frames = edges.map((x, i) => {
    acc += widths[i]!;
    return {
      clipPath: `inset(-4px calc(100% - ${x}ch) -4px -4px)`,
      offset: Math.min(1, acc / total),
      easing: "step-end",
    };
  });
  for (const a of stage.getAnimations()) a.cancel();
  stage.animate(frames, { duration: total * cell, delay, fill: "backwards" });
  return total * cell + delay;
}

/** Restart a one-shot CSS animation class. */
export function pulse(el: Element | null | undefined, cls: string, hold = 700): void {
  if (!el) return;
  el.classList.remove(cls);
  void (el as HTMLElement).offsetWidth;
  el.classList.add(cls);
  setTimeout(() => el.classList.remove(cls), hold);
}

/** Move a marker (the search selection bar, the "on this page" bar) over a target in the same offset parent. */
export function slideTo(ink: HTMLElement | null, target: HTMLElement | null | undefined): void {
  if (!ink) return;
  if (!target) {
    ink.style.height = "0px";
    return;
  }
  ink.style.transform = `translateY(${target.offsetTop}px)`;
  ink.style.height = `${target.offsetHeight}px`;
}
