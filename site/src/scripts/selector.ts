// dbt selector grammar over a graph, with the same rules as `dbtl ls`.
//
//   spec    := term (WS term)*            whitespace = union
//   term    := factor ("," factor)*       comma = intersection
//   factor  := [N] "+"? atom "+"?         graph hops
//   atom    := tag:x | path:x | config.materialized:x | exposure:x | meta:k[=v] | name or glob
//
// Names take the globs `*`, `?`, `[set]` and `[!set]` (fnmatch rules, case-sensitive).
// `state:` atoms need a --diff baseline, which a browser page does not have, so they raise a
// message instead of matching nothing. Malformed specs raise the same errors the CLI reports.
import type { Graph, GraphNode } from "./graph.ts";
import { walk } from "./graph.ts";

export interface SelectorNode extends GraphNode {
  /** Resource type: model, source, seed, snapshot, exposure. */
  type: string;
  /** Materialization of models, seeds and snapshots. */
  mat: string | null;
  tags: string[];
  /** Path relative to the resource root. `path:` is a substring match on it. */
  selPath: string;
  /** `meta` values as strings. */
  meta: Record<string, string>;
}

export class SelectorError extends Error {}

const isDigits = (s: string) => /^[0-9]*$/.test(s);

/** Translate an fnmatch pattern to an anchored regular expression. */
function globRegex(pattern: string): RegExp {
  const lit = (c: string) => c.replace(/[.*+?^${}()|[\]\\/-]/g, "\\$&");
  let out = "^";
  const chars = Array.from(pattern);
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i];
    if (c === "*") out += ".*";
    else if (c === "?") out += ".";
    else if (c === "[") {
      let negated = false;
      if (chars[i + 1] === "!") {
        negated = true;
        i++;
      }
      // The class body ends at the first `]` that is not its first character.
      let body = "";
      let first = true;
      let closed = false;
      for (i++; i < chars.length; i++) {
        if (chars[i] === "]" && !first) {
          closed = true;
          break;
        }
        first = false;
        body += chars[i];
      }
      if (closed) {
        out += `[${negated ? "^" : ""}${body.replace(/[[\\\]^]/g, "\\$&")}]`;
      } else {
        // An unmatched `[` is a literal.
        out += `\\[${negated ? "!" : ""}${lit(body)}`;
      }
    } else out += lit(c);
  }
  return new RegExp(`${out}$`, "s");
}

/** The atom a factor carries: its leading `+` / `N+` and trailing `+` removed. */
function factorAtom(factor: string): string {
  let rest = factor;
  const at = factor.indexOf("+");
  if (at >= 0) {
    const lead = factor.slice(0, at);
    const tail = factor.slice(at + 1);
    if (lead === "" || isDigits(lead)) rest = tail;
    else if (tail === "") rest = lead;
  }
  return rest.endsWith("+") ? rest.slice(0, -1) : rest;
}

function validateAtom(atom: string): void {
  if (atom.startsWith("config.")) {
    const c = atom.slice("config.".length);
    const at = c.indexOf(":");
    const key = at < 0 ? null : c.slice(0, at);
    const value = at < 0 ? "" : c.slice(at + 1);
    if (key !== "materialized") {
      throw new SelectorError(`unknown config selector 'config.${c}' (supported: config.materialized:<value>)`);
    }
    if (value === "") throw new SelectorError("config.materialized: needs a non-empty value");
    return;
  }
  if (atom.startsWith("meta:")) {
    const m = atom.slice("meta:".length);
    const key = m.includes("=") ? m.slice(0, m.indexOf("=")) : m;
    if (key === "") throw new SelectorError("meta: needs a key (meta:<key> or meta:<key>=<value>)");
    return;
  }
  const at = atom.indexOf(":");
  if (at < 0) return; // bare name or glob
  const method = atom.slice(0, at);
  const value = atom.slice(at + 1);
  if (method === "tag" || method === "path" || method === "exposure") {
    if (value === "") throw new SelectorError(`${method}: needs a non-empty value`);
    return;
  }
  if (method === "state") {
    if (value === "modified" || value === "new") {
      throw new SelectorError(
        `state: selectors compare against a --diff baseline, and the playground has none. Run it locally, for example: dbtl ls 'state:modified+' --diff git:main`,
      );
    }
    throw new SelectorError(`unknown state selector 'state:${value}' (supported: modified, new)`);
  }
  throw new SelectorError(
    `unknown selector method '${method}:' in '${atom}' (supported: tag:, path:, config.materialized:, exposure:, meta:, or a node name; state: needs --diff)`,
  );
}

function validateFactor(factor: string): void {
  if (factor === "") throw new SelectorError("an empty selector term (a ',' needs a selector on both sides)");
  const at = factor.indexOf("+");
  if (at >= 0) {
    const lead = factor.slice(0, at);
    const rest = factor.slice(at + 1);
    if (lead !== "" && !isDigits(lead) && rest !== "") {
      throw new SelectorError(`invalid hop '${lead}+' in '${factor}' — hop counts must be a number`);
    }
  }
  const atom = factorAtom(factor);
  if (atom === "") throw new SelectorError(`'${factor}' — a '+' hop needs a selector to expand`);
  if (atom.includes("+")) {
    throw new SelectorError(`invalid factor '${factor}' — only one leading and one trailing '+' hop are allowed`);
  }
  validateAtom(atom);
}

/** Throws a SelectorError for anything `dbtl ls` would reject. */
export function validate(spec: string): void {
  if (spec.trim() === "") throw new SelectorError("empty selector");
  for (const term of spec.trim().split(/\s+/)) for (const factor of term.split(",")) validateFactor(factor);
}

function atomSet(g: Graph<SelectorNode>, atom: string): Set<string> {
  const out = new Set<string>();
  const add = (pred: (n: SelectorNode) => boolean) => {
    for (const n of g.nodes) if (pred(n)) out.add(n.id);
  };
  if (atom.startsWith("config.")) {
    const m = atom.slice("config.materialized:".length);
    add((n) => n.mat === m);
    return out;
  }
  if (atom.startsWith("meta:")) {
    const m = atom.slice("meta:".length);
    const eq = m.indexOf("=");
    const key = eq < 0 ? m : m.slice(0, eq);
    const val = eq < 0 ? null : m.slice(eq + 1);
    add((n) => Object.hasOwn(n.meta, key) && (val === null || n.meta[key] === val));
    return out;
  }
  const at = atom.indexOf(":");
  const method = at < 0 ? "" : atom.slice(0, at);
  const value = at < 0 ? "" : atom.slice(at + 1);
  if (method === "tag") add((n) => n.tags.includes(value));
  else if (method === "path") add((n) => n.selPath.includes(value));
  else if (method === "exposure") {
    // The named exposure, everything upstream of it, and the exposure itself.
    for (const n of g.nodes) {
      if (n.type === "exposure" && n.name === value) {
        out.add(n.id);
        for (const up of walk(g, n.id, "up")) out.add(up);
      }
    }
  } else if (/[*?[]/.test(atom)) {
    const re = globRegex(atom);
    add((n) => re.test(n.name));
  } else add((n) => n.name === atom);
  return out;
}

function factorSet(g: Graph<SelectorNode>, factor: string): Set<string> {
  let upHops = 0;
  let rest = factor;
  const at = factor.indexOf("+");
  if (at >= 0) {
    const lead = factor.slice(0, at);
    if (lead === "") {
      upHops = Infinity;
      rest = factor.slice(at + 1);
    } else if (isDigits(lead)) {
      upHops = Number.parseInt(lead, 10);
      rest = factor.slice(at + 1);
    }
  }
  const down = rest.endsWith("+");
  const base = atomSet(g, down ? rest.slice(0, -1) : rest);
  if (upHops === 0 && !down) return base;
  const out = new Set(base);
  for (const id of base) {
    if (upHops > 0) for (const x of walk(g, id, "up", upHops)) out.add(x);
    if (down) for (const x of walk(g, id, "down")) out.add(x);
  }
  return out;
}

/** Evaluate `spec` to the set of matching node ids. Throws a SelectorError on a spec `dbtl ls` rejects. */
export function evaluate(g: Graph<SelectorNode>, spec: string): Set<string> {
  validate(spec);
  const result = new Set<string>();
  for (const term of spec.trim().split(/\s+/)) {
    let acc: Set<string> | null = null;
    for (const factor of term.split(",")) {
      const set: Set<string> = factorSet(g, factor);
      const prev = acc as Set<string> | null;
      acc = prev === null ? set : new Set([...prev].filter((x) => set.has(x)));
    }
    for (const x of acc ?? []) result.add(x);
  }
  return result;
}
