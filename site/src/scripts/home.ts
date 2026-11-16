// The home page demo: a model list, a lineage with a cursor, and an inspector, on a sample
// project. The server draws the first state; this makes it live. Keys and clicks follow the TUI:
// h j k l move the cursor, Enter re-roots on it, Esc returns it to the root. The lineage comes
// from the layouts the engine produced at build time (home-lineage.json) — fetched once and
// repainted per re-root, never computed here.
import { jaffle_finance } from "../data/graph.json";
import lineageUrl from "../data/home-lineage.json?url";
import { playWhenVisible } from "./boot.ts";
import { cells, type Lin, type LineageData } from "./engine-lineage.ts";
import { index } from "./graph.ts";
import {
  demoStyle,
  HOME,
  homeIds,
  impactText,
  inspectorHtml,
  pathSet,
  titleHtml,
} from "./home-view.ts";
import { mount } from "./lineage-view.ts";
import { toProject, type RawSample } from "./samples.ts";

const project = toProject(jaffle_finance as RawSample);
const ix = index(project);
const ids = homeIds(project);
const names = new Map(project.nodes.map((n) => [n.id, n.name] as const));

const panel = document.getElementById("hero-tui");
const host = document.getElementById("hero-lin");
const scroller = document.getElementById("hero-scroll");
const list = document.getElementById("hero-list");

async function init(): Promise<void> {
  if (!panel || !host || !scroller || !list) return;
  const data = (await (await fetch(lineageUrl)).json()) as {
    built?: boolean;
    layouts?: Record<string, LineageData>;
  };
  if (!data.built || !data.layouts) return; // the SSR'd notice stands

  const cache = new Map<string, Lin>();
  const linOf = (root: string): Lin | null => {
    let L = cache.get(root);
    if (L) return L;
    const d = data.layouts![root];
    if (!d) return null;
    L = cells(d, names);
    cache.set(root, L);
    return L;
  };

  let root = ids.root;
  let cursor = ids.trail[ids.trail.length - 1]!;
  let previous = pathSet(project, root, cursor);
  let touched = false;

  const view = mount(host, {
    onPick: (id) => pick(id),
    onCommit: (id) => reroot(id),
  });

  function render(reveal: { speed?: number; delay?: number } | null = null): number {
    const L = linOf(root);
    if (!L) return 0;
    if (!L.rects[cursor]) cursor = root;
    const path = pathSet(project, root, cursor);
    const was = previous;
    const total = view.render(L, demoStyle(root, cursor, path, was), {
      cursor: cursor !== root ? cursor : null,
      reveal,
    });
    previous = path;
    const rootNode = ix.by.get(root)!;
    document.getElementById("hero-title")!.innerHTML = titleHtml(rootNode.name, HOME.depth);
    document.getElementById("hero-tab")!.textContent = `${rootNode.name} ×`;
    list!
      .querySelectorAll("button")
      .forEach((b) => b.setAttribute("aria-current", String(b.dataset.id === root)));
    showCurrentModel();
    document.getElementById("hero-impact")!.textContent = impactText(project, cursor);
    document.getElementById("hero-insp")!.innerHTML = inspectorHtml(project, cursor);
    return total;
  }

  // Scroll the list itself (never the page) so the root's button is in view.
  function showCurrentModel(): void {
    const el = list!.querySelector<HTMLElement>('button[aria-current="true"]');
    if (!el) return;
    if (
      el.offsetTop < list!.scrollTop ||
      el.offsetTop + el.offsetHeight > list!.scrollTop + list!.clientHeight
    ) {
      list!.scrollTop = Math.max(0, el.offsetTop - list!.clientHeight / 2);
    }
  }

  function pick(id: string): void {
    touched = true;
    cursor = id;
    render();
  }

  function reroot(id: string): void {
    touched = true;
    root = cursor = id;
    previous = new Set();
    render({ speed: 0.35 });
  }

  list.addEventListener("click", (ev) => {
    const b = (ev.target as Element).closest("button");
    if (b?.dataset.id) reroot(b.dataset.id);
  });

  scroller.addEventListener("keydown", (ev) => {
    if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
    const k = ev.key;
    if (k === "Enter") {
      ev.preventDefault();
      if (cursor !== root) reroot(cursor);
      return;
    }
    if (k === "Escape" && cursor !== root) {
      ev.preventDefault();
      pick(root);
      return;
    }
    let dir: [number, number] | null = null;
    if (k === "h" || k === "ArrowLeft") dir = [-1, 0];
    else if (k === "l" || k === "ArrowRight") dir = [1, 0];
    else if (k === "k" || k === "ArrowUp") dir = [0, -1];
    else if (k === "j" || k === "ArrowDown") dir = [0, 1];
    if (!dir) return;
    ev.preventDefault();
    ev.stopPropagation();
    const L = view.layout;
    const here = L?.rects?.[cursor];
    if (!here) return;
    let next = cursor;
    if (dir[0]) {
      // The node in the neighbouring column whose row is nearest to the cursor's.
      const column = L.columns[here.col + dir[0]];
      if (!column) return;
      next = column.reduce(
        (best, id) =>
          Math.abs(L.rects[id]!.y - here.y) < Math.abs(L.rects[best]!.y - here.y) ? id : best,
        column[0]!,
      );
    } else {
      const column = L.columns[here.col]!;
      const i = column.indexOf(cursor) + dir[1];
      if (i < 0 || i >= column.length) return;
      next = column[i]!;
    }
    pick(next);
    const el = view.pre.querySelector(".cur") ?? view.pre.querySelector(".root");
    el?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });

  render();

  playWhenVisible({
    panel,
    trail: ids.trail,
    touched: () => touched,
    start: (delay) => {
      cursor = root;
      previous = new Set();
      return render({ delay });
    },
    visit: (id) => {
      cursor = id;
      render();
    },
  });
}

// The SSR'd diagram stays useful even if the fetch fails, so this is quiet.
init().catch(() => {});
