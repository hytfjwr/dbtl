// The engine host. One engine lives inside this worker; the page talks to it
// by message, so a heavy lint run never blocks the UI thread and a wasm trap
// takes down only the worker — the page respawns and replays its input.
import { loadEngine, type Engine } from "../engine/index.ts";

interface Req {
  id: number;
  op: string;
  dir?: string;
  input?: unknown;
}

interface Res {
  id: number;
  ok: boolean;
  result?: unknown;
  error?: string;
  fatal?: boolean;
}

let engineP: Promise<Engine> | null = null;
let dead = false;

// A wasm-side failure is unrecoverable — the page must respawn the worker.
// Usage errors (bad selector, bad config, unknown node) come back as plain
// Errors with the CLI's wording and are not fatal.
const isFatal = (e: unknown): boolean =>
  e instanceof WebAssembly.RuntimeError ||
  (e instanceof Error &&
    /unreachable|out of bounds|memory access|recursive use|table index|maximum call stack/i.test(
      e.message,
    ));

const post = (m: Res) => self.postMessage(m);

const run = async (op: string, input: unknown, dir?: string): Promise<unknown> => {
  if (op === "init") {
    engineP = loadEngine(dir ? { dir } : {});
    await engineP;
    return null;
  }
  if (!engineP) throw new Error("engine is not initialized — send init first");
  const engine = await engineP;
  switch (op) {
    case "load":
      return engine.load(input as Parameters<Engine["load"]>[0]);
    case "select":
      return engine.select(input as string);
    case "nodeDetail":
      return engine.nodeDetail(input as string);
    case "lineage":
      return engine.lineage(input as Parameters<Engine["lineage"]>[0]);
    case "lint":
      return engine.lint(input as Parameters<Engine["lint"]>[0]);
    case "columns":
      return engine.columns(input as Parameters<Engine["columns"]>[0]);
    case "replaceSql": {
      const { uid, sql } = input as { uid: string; sql: string };
      return engine.replaceSql(uid, sql);
    }
    case "revertSql":
      return engine.revertSql(input as string);
    default:
      throw new Error(`unknown op ${op}`);
  }
};

self.onmessage = (ev: MessageEvent<Req>) => {
  const { id, op, dir, input } = ev.data;
  run(op, input, dir).then(
    (result) => post({ id, ok: true, result }),
    (e) => {
      const fatal = dead || isFatal(e);
      if (fatal) dead = true;
      post({ id, ok: false, error: e instanceof Error ? e.message : String(e), fatal });
    },
  );
};
