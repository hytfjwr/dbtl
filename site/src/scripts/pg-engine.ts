// The browser-side engine client: owns the worker, answers calls, and when the
// wasm traps, respawns the worker and replays the last load plus every SQL edit
// so the page comes back to where it was instead of dying with the engine.
import type {
  ColumnsInput,
  ColumnsOutput,
  DetailOutput,
  LineageInput,
  LineageOutput,
  LintOptions,
  LintOutput,
  LoadInput,
  LoadOutput,
  SelectOutput,
} from "../engine/index.ts";

export type Status =
  | { kind: "starting" }
  | { kind: "ready" }
  | { kind: "restarting"; reason: string }
  | { kind: "failed"; reason: string };

interface Pending {
  res: (v: unknown) => void;
  rej: (e: Error) => void;
}

/** Give up after this many traps in a row — a manifest that keeps crashing won't loop forever. */
const MAX_CRASHES = 2;

export class EngineClient {
  private worker: Worker | null = null;
  private seq = 0;
  private gen = 0;
  private pending = new Map<number, Pending>();
  private readyP: Promise<void> = Promise.resolve();
  private lastLoad: LoadInput | null = null;
  private edits = new Map<string, string>();
  private crashes = 0;
  private deadReason = "";

  constructor(
    private dir: string,
    private onStatus: (s: Status) => void,
    private onReady: () => void,
  ) {
    this.respawn();
  }

  // --- worker lifecycle ---------------------------------------------------

  private respawn(): void {
    const gen = ++this.gen;
    this.onStatus(
      this.crashes ? { kind: "restarting", reason: this.deadReason } : { kind: "starting" },
    );
    const w = new Worker(new URL("./pg-worker.ts", import.meta.url), { type: "module" });
    this.worker = w;
    w.onmessage = (ev: MessageEvent) => this.onMessage(gen, ev);
    w.onerror = (ev) => this.onCrash(gen, `worker error: ${ev.message ?? "unknown"}`);

    this.readyP = (async () => {
      await this.rpc(w, "init", undefined);
      await this.restore(w);
      if (gen !== this.gen) {
        w.terminate();
        return;
      }
      this.crashes = 0;
      this.onStatus({ kind: "ready" });
      this.onReady();
    })();
    this.readyP.catch((e) => {
      if (gen === this.gen && this.worker === w) {
        this.onStatus({ kind: "failed", reason: e instanceof Error ? e.message : String(e) });
      }
    });
  }

  /** Replay the last successful load and every SQL edit onto a fresh engine. */
  private async restore(w: Worker): Promise<void> {
    if (!this.lastLoad) return;
    await this.rpc(w, "load", this.lastLoad);
    for (const [uid, sql] of this.edits) {
      await this.rpc(w, "replaceSql", { uid, sql });
    }
  }

  private onMessage(gen: number, ev: MessageEvent): void {
    if (gen !== this.gen) return;
    const { id, ok, result, error, fatal } = ev.data as {
      id: number;
      ok: boolean;
      result?: unknown;
      error?: string;
      fatal?: boolean;
    };
    const p = this.pending.get(id);
    if (!p) return;
    this.pending.delete(id);
    if (ok) {
      p.res(result);
    } else {
      const err = new Error(error ?? "engine call failed");
      if (fatal) {
        this.onCrash(gen, err.message);
        err.name = "EngineCrash";
      }
      p.rej(err);
    }
  }

  private onCrash(gen: number, reason: string): void {
    if (gen !== this.gen) return;
    this.deadReason = reason;
    this.worker?.terminate();
    this.worker = null;
    for (const p of this.pending.values()) {
      p.rej(Object.assign(new Error("the engine restarted — retry"), { name: "EngineCrash" }));
    }
    this.pending.clear();
    this.crashes += 1;
    if (this.crashes > MAX_CRASHES) {
      this.onStatus({ kind: "failed", reason });
      this.readyP = Promise.reject(new Error(`engine keeps failing: ${reason}`));
      this.readyP.catch(() => {});
      return;
    }
    this.respawn();
  }

  // --- calls ---------------------------------------------------------------

  private rpc(w: Worker, op: string, input: unknown): Promise<unknown> {
    const id = ++this.seq;
    return new Promise((res, rej) => {
      this.pending.set(id, { res, rej });
      w.postMessage({ id, op, dir: this.dir, input });
    });
  }

  private async call(op: string, input: unknown): Promise<unknown> {
    await this.readyP;
    const w = this.worker;
    if (!w) throw Object.assign(new Error("engine is down"), { name: "EngineCrash" });
    return this.rpc(w, op, input);
  }

  /** Load a project: manifest text plus the .yml files lint excerpts read. */
  async load(input: LoadInput): Promise<LoadOutput> {
    const out = (await this.call("load", input)) as LoadOutput;
    // Only a load that succeeded is worth replaying after a crash.
    this.lastLoad = input;
    this.edits.clear();
    return out;
  }

  select(spec: string): Promise<SelectOutput> {
    return this.call("select", spec) as Promise<SelectOutput>;
  }

  nodeDetail(uid: string): Promise<DetailOutput> {
    return this.call("nodeDetail", uid) as Promise<DetailOutput>;
  }

  lineage(input: LineageInput): Promise<LineageOutput> {
    return this.call("lineage", input) as Promise<LineageOutput>;
  }

  lint(input: LintOptions): Promise<LintOutput> {
    return this.call("lint", input) as Promise<LintOutput>;
  }

  columns(input: ColumnsInput): Promise<ColumnsOutput> {
    return this.call("columns", input) as Promise<ColumnsOutput>;
  }

  async replaceSql(uid: string, sql: string): Promise<void> {
    await this.call("replaceSql", { uid, sql });
    this.edits.set(uid, sql);
  }

  async revertSql(uid: string): Promise<void> {
    await this.call("revertSql", uid);
    this.edits.delete(uid);
  }

  dispose(): void {
    this.gen += 1;
    this.worker?.terminate();
    this.worker = null;
  }
}
