// The one place the docs site loads the WebAssembly engine — browser,
// Web Worker, and Node all come through here.
//
// The generated module lives in public/engine/ (scripts/build-engine.mjs
// writes it; it is gitignored and shipped as a static file), so browsers
// and workers import it by URL and the platform instantiates it, while
// Node imports the file URL and feeds the bytes to `initSync` — the two
// entry points every generated wasm-bindgen --target web module exports.
//
// The wire contract below is maintained by hand: methods take a plain
// object, return the parsed JSON the engine emits, and throw whatever
// `Error` the engine throws (its message is the CLI's wording). Adding a
// method? Mirror the engine's serde shape exactly.
//
// Keep this file erasable-syntax-only (interfaces, types, `as`) — Node's
// type stripping loads it directly in scripts/check-engine.mjs.

export interface LoadInput {
  /** The manifest.json contents. */
  manifest: string;
  /** project-relative .yml path → file text (lint positions/excerpts). */
  files?: Record<string, string>;
  /** Keep installed-package nodes (default: root project only). */
  include_packages?: boolean;
}

export interface LoadOutput {
  project: string;
  counts: Record<string, number>;
  nodes: number;
  warnings: string[];
}

export interface SelectedNode {
  unique_id: string;
  name: string;
  resource_type: string;
  materialized: string | null;
  /** Project-relative file the node is declared in, when known. */
  path: string | null;
}

export interface SelectOutput {
  nodes: SelectedNode[];
  warnings: string[];
}

export interface DetailOutput {
  unique_id: string;
  name: string;
  resource_type: string;
  materialized: string | null;
  path: string | null;
  raw_code: string | null;
  has_compiled_code: boolean;
  columns: string[];
}

export type Direction = "both" | "upstream" | "downstream";
export type GlyphMode = "unicode" | "ascii";
export type Density = "comfortable" | "compact";

export interface LineageInput {
  /** One node's neighborhood; wins over `roots` when both are set. */
  root?: string | null;
  /** Nodes whose neighborhoods merge into one diagram. */
  roots?: string[];
  direction?: Direction;
  depth?: number | null;
  glyphs?: GlyphMode;
  density?: Density;
}

export interface LineageRect {
  uid: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface LineageOutput {
  width: number;
  height: number;
  /** One string per row, `width` chars each. */
  rows: string[];
  /**
   * Per-cell materialization class — ' ' plain, 't' table, 'v' view,
   * 'i' incremental, 'e' ephemeral, 'm' other model, 's' source, 'd' seed,
   * 'n' snapshot, 'x' exposure, 'c' collapse marker.
   */
  classes: string[];
  /** Per-cell emphasis flag ('1' = the selected node's label). */
  emphasis: string[];
  rects: LineageRect[];
  selected: string | null;
}

export type FailOn = "error" | "warn" | "never";

export interface LintOptions {
  /** .dbtl.yml contents (default: shipped defaults). */
  config?: string | null;
  /** A dbt selector narrowing findings to matched nodes. */
  selector?: string | null;
  fail_on?: FailOn | null;
  // There is no baseline input: the run always behaves like `dbtl lint
  // --no-baseline` — a baseline freezes findings from a real checkout,
  // and the engine has no filesystem to read one from.
}

export interface LintOutput {
  /** `dbtl lint --format text --color never` stdout, byte for byte. */
  text: string;
  /** The same body with ANSI styling (`--color always`). */
  ansi: string;
  /** `dbtl lint --format json` stdout, byte for byte. */
  json: string;
  /** 0 pass, 1 gate fail. */
  exit_code: number;
  error: string | null;
  warnings: string[];
}

export interface ColumnsInput {
  /** `<model>.<column>` — model is a unique_id or a unique node name. */
  target: string;
  /** Provenance instead of blast radius. */
  upstream?: boolean;
}

export interface ColumnsOutput {
  /** `dbtl columns --format json` stdout, byte for byte. */
  json: string;
  /** The default Markdown table output. */
  markdown: string;
  warnings: string[];
}

export interface ChangeOutput {
  changed: boolean;
}

/** The generated module's exports (wasm-bindgen --target web). */
interface Bindings {
  /** Async init: accepts a URL/string it fetches itself. */
  default: (module_or_path?: unknown) => Promise<unknown>;
  /** Sync init: accepts the wasm bytes directly. */
  initSync: (module: unknown) => unknown;
  WasmEngine: new () => RawEngine;
}

/** The wasm-bindgen class — JSON strings in, JSON strings out. */
interface RawEngine {
  load(input: string): string;
  select(input: string): string;
  nodeDetail(input: string): string;
  lineage(input: string): string;
  lint(input: string): string;
  columns(input: string): string;
  replaceSql(input: string): string;
  revertSql(input: string): string;
  /** Release the wasm-side allocation (wasm-bindgen's generated free). */
  free(): void;
}

/**
 * Thrown when public/engine/ has no generated module — either the build
 * ran with DBTL_SKIP_ENGINE=1 or the wasm toolchain was missing. Catch it
 * to show "the engine is not built" instead of a generic load failure.
 */
export class EngineNotBuiltError extends Error {
  /** The directory or URL the module was looked up in. */
  readonly dir: string;

  constructor(dir: string, cause: unknown) {
    super(
      `the dbtl engine is not built — run \`node scripts/build-engine.mjs\` in site/ ` +
        `(needs cargo, the wasm32-unknown-unknown target, and wasm-bindgen-cli); ` +
        `looked in ${dir}`,
      { cause },
    );
    this.name = "EngineNotBuiltError";
    this.dir = dir;
  }
}

/**
 * A loaded-and-initialized engine. `load()` must be the first call — it
 * parses the manifest; every other method throws until it succeeds.
 */
export class Engine {
  readonly #raw: RawEngine;

  constructor(raw: RawEngine) {
    this.#raw = raw;
  }

  load(input: LoadInput): LoadOutput {
    return JSON.parse(this.#raw.load(JSON.stringify(input))) as LoadOutput;
  }

  select(selector: string): SelectOutput {
    return JSON.parse(this.#raw.select(JSON.stringify({ selector }))) as SelectOutput;
  }

  nodeDetail(uniqueId: string): DetailOutput {
    return JSON.parse(
      this.#raw.nodeDetail(JSON.stringify({ unique_id: uniqueId })),
    ) as DetailOutput;
  }

  lineage(input: LineageInput): LineageOutput {
    return JSON.parse(this.#raw.lineage(JSON.stringify(input))) as LineageOutput;
  }

  lint(input: LintOptions = {}): LintOutput {
    return JSON.parse(this.#raw.lint(JSON.stringify(input))) as LintOutput;
  }

  columns(input: ColumnsInput): ColumnsOutput {
    return JSON.parse(this.#raw.columns(JSON.stringify(input))) as ColumnsOutput;
  }

  replaceSql(uniqueId: string, sql: string): ChangeOutput {
    return JSON.parse(
      this.#raw.replaceSql(JSON.stringify({ unique_id: uniqueId, sql })),
    ) as ChangeOutput;
  }

  revertSql(uniqueId: string): ChangeOutput {
    return JSON.parse(this.#raw.revertSql(JSON.stringify({ unique_id: uniqueId }))) as ChangeOutput;
  }

  /**
   * Free the wasm-side engine. Pages that drop the engine on navigation
   * can skip this — the allocation dies with the page — but a long-lived
   * page that loads many manifests should dispose of each engine it
   * replaces.
   */
  dispose(): void {
    this.#raw.free();
  }
}

const isNode =
  typeof (globalThis as { process?: { versions?: { node?: string } } }).process?.versions?.node ===
  "string";

// The directory public/engine/ is served at — under the site's configured
// base (Astro sets BASE_URL; plain Node has no import.meta.env).
function defaultDir(): string {
  const env = (import.meta as unknown as { env?: { BASE_URL?: string } }).env;
  return `${env?.BASE_URL ?? "/"}engine/`;
}

// The module's file names — dir arguments are treated as a prefix and
// may arrive with or without the trailing slash.
const JS_NAME = "dbtl_wasm.js";
const WASM_NAME = "dbtl_wasm_bg.wasm";

function joinUrl(dir: string, name: string): string {
  return `${dir.endsWith("/") ? dir : `${dir}/`}${name}`;
}

async function importBrowser(dir: string): Promise<Bindings> {
  let mod: Bindings;
  try {
    mod = (await import(/* @vite-ignore */ joinUrl(dir, JS_NAME))) as Bindings;
  } catch (e) {
    throw new EngineNotBuiltError(dir, e);
  }
  try {
    await mod.default(joinUrl(dir, WASM_NAME));
  } catch (e) {
    // The .js loaded but the .wasm fetch/instantiate failed — a
    // half-staged public/engine/ reads exactly like "not built".
    throw new EngineNotBuiltError(dir, e);
  }
  return mod;
}

async function importNode(dir: string): Promise<Bindings> {
  const [{ pathToFileURL }, { resolve }, { readFile }] = await Promise.all([
    import("node:url"),
    import("node:path"),
    import("node:fs/promises"),
  ]);
  const abs = resolve(dir);
  let mod: Bindings;
  try {
    mod = (await import(pathToFileURL(resolve(abs, JS_NAME)).href)) as Bindings;
  } catch (e) {
    throw new EngineNotBuiltError(dir, e);
  }
  try {
    mod.initSync({ module: await readFile(resolve(abs, WASM_NAME)) });
  } catch (e) {
    throw new EngineNotBuiltError(dir, e);
  }
  return mod;
}

/**
 * Load and initialize the engine. `dir` overrides where the generated
 * module is read from: a URL prefix in the browser (default `<base>engine/`),
 * a filesystem directory in Node (default `public/engine/` of the site
 * source tree — resolved from this file).
 */
export async function loadEngine(opts: { dir?: string } = {}): Promise<Engine> {
  let dir = opts.dir;
  if (!dir) {
    if (isNode) {
      const { fileURLToPath } = await import("node:url");
      // site/src/engine/index.ts → site/public/engine/
      dir = fileURLToPath(new URL("../../public/engine/", import.meta.url));
    } else {
      dir = defaultDir();
    }
  }
  const mod = await (isNode ? importNode(dir) : importBrowser(dir));
  return new Engine(new mod.WasmEngine());
}
