// Graph helpers shared by the lineage renderer, the selector engine, the home
// demo and the playground. A graph is {nodes, edges}; an edge is [parent, child].

export interface GraphNode {
  id: string;
  name: string;
}

export interface Graph<N extends GraphNode = GraphNode> {
  nodes: N[];
  edges: [string, string][];
}

interface Index<N extends GraphNode> {
  by: Map<string, N>;
  up: Map<string, string[]>;
  down: Map<string, string[]>;
}

const cache = new WeakMap<object, Index<GraphNode>>();

export function index<N extends GraphNode>(g: Graph<N>): Index<N> {
  const hit = cache.get(g);
  if (hit) return hit as Index<N>;
  const by = new Map(g.nodes.map((n) => [n.id, n] as const));
  const up = new Map<string, string[]>();
  const down = new Map<string, string[]>();
  for (const n of g.nodes) {
    up.set(n.id, []);
    down.set(n.id, []);
  }
  for (const [p, c] of g.edges) {
    if (by.has(p) && by.has(c)) {
      up.get(c)!.push(p);
      down.get(p)!.push(c);
    }
  }
  const ix = { by, up, down };
  cache.set(g, ix);
  return ix;
}

/** Nodes reachable from `id` in one direction, at most `max` hops away. `id` itself is not included. */
export function walk(g: Graph, id: string, dir: "up" | "down", max = Infinity): Set<string> {
  const ix = index(g);
  const rel = dir === "up" ? ix.up : ix.down;
  const out = new Set<string>();
  let frontier = [id];
  for (let depth = 0; depth < max && frontier.length > 0; depth++) {
    const next: string[] = [];
    for (const cur of frontier) {
      for (const x of rel.get(cur) ?? []) {
        if (!out.has(x)) {
          out.add(x);
          next.push(x);
        }
      }
    }
    frontier = next;
  }
  return out;
}

/** The subgraph induced by `ids`. */
export function sub<N extends GraphNode>(g: Graph<N>, ids: Set<string>): Graph<N> {
  return {
    nodes: g.nodes.filter((n) => ids.has(n.id)),
    edges: g.edges.filter(([p, c]) => ids.has(p) && ids.has(c)),
  };
}

/** Every node on a path between a and b, in either direction. Empty when they are unrelated. */
export function between(g: Graph, a: string, b: string): Set<string> {
  const s = new Set<string>();
  const upA = walk(g, a, "up");
  const downA = walk(g, a, "down");
  if (upA.has(b)) {
    const d = walk(g, b, "down");
    for (const x of upA) if (d.has(x)) s.add(x);
  } else if (downA.has(b)) {
    const u = walk(g, b, "up");
    for (const x of downA) if (u.has(x)) s.add(x);
  } else {
    return s;
  }
  s.add(a);
  s.add(b);
  return s;
}
