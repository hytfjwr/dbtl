// The playground: the real dbtl engine runs in a Web Worker and drives every
// pane — selector matches, the engine's lineage layout, the lint report,
// column lineage, and the SQL / .dbtl.yml editors. The page was rendered by
// the same engine in Node; this replaces that static result once it is ready.
import type {
  ColumnsOutput,
  DetailOutput,
  LineageOutput,
  LintOutput,
  LoadInput,
  SelectedNode,
} from "../engine/index.ts";
import { esc } from "./html.ts";
import { EngineClient, type Status } from "./pg-engine.ts";
import {
  columnOptionsHtml,
  columnsBodyHtml,
  columnsCommand,
  CONFIG_TEMPLATE,
  EXAMPLES,
  GENERIC_EXAMPLES,
  lintBodyHtml,
  lintCommand,
  lintCount,
  lintFootHtml,
  lsCommand,
  matchedHtml,
  matchedTitle,
  nodeOptionsHtml,
  stageHtml,
} from "./pg-render.ts";

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel);

const ctl = $("#pg-drop");
const input = $<HTMLInputElement>("#pg-sel");
const linHost = $("#pg-lin");

if (ctl && input && linHost) {
  const dir = ctl.dataset.engineDir ?? "/engine/";
  const samplesDir = dir.replace(/engine\/?$/, "samples/");

  /** Uploaded manifests stay under this size — the wasm heap is finite. */
  const MAX_MANIFEST = 16 * 1024 * 1024;

  const S = {
    /** Sample name, or "__custom__" for an uploaded manifest. */
    project: "jaffle_finance",
    custom: null as { fileName: string; input: LoadInput; nodes: number } | null,
    spec: input.value,
    total: 0,
    matched: [] as SelectedNode[],
    hit: new Map<string, string>(),
    prevHit: null as Set<string> | null,
    selectError: "",
    lineage: null as LineageOutput | null,
    lint: null as LintOutput | null,
    lintError: "",
    loadWarnings: [] as string[],
    picked: null as string | null,
    pickedName: "",
    detail: null as DetailOutput | null,
    edited: new Set<string>(),
    cfg: CONFIG_TEMPLATE,
    nodesAll: null as SelectedNode[] | null,
    colNode: null as string | null,
    colCol: null as string | null,
    colCols: [] as string[],
    colUp: false,
    cols: null as ColumnsOutput | null,
    colsError: "",
  };

  // --- plumbing ------------------------------------------------------------

  const statusEl = $("#pg-status")!;
  const statusText = (s: Status): string => {
    switch (s.kind) {
      case "starting":
        return "engine: starting…";
      case "ready":
        return "engine: ready — WebAssembly, in your browser";
      case "restarting":
        return "engine: restarting after a crash…";
      case "failed":
        return `engine: failed — ${s.reason}`;
    }
  };

  /** A wasm trap means the client already restarted — the job just aborts. */
  const transient = (e: unknown): boolean => e instanceof Error && e.name === "EngineCrash";
  const msg = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  const engine = new EngineClient(
    dir,
    (s) => {
      statusEl.textContent = statusText(s);
    },
    () => {
      void connect();
    },
  );

  const sampleCache = new Map<string, Promise<LoadInput>>();
  const fetchSample = (name: string): Promise<LoadInput> => {
    let p = sampleCache.get(name);
    if (!p) {
      p = (async () => {
        const [m, f] = await Promise.all([
          fetch(`${samplesDir}${name}/manifest.json`),
          fetch(`${samplesDir}${name}/files.json`),
        ]);
        if (!m.ok || !f.ok) throw new Error(`sample '${name}' could not be fetched`);
        return { manifest: await m.text(), files: (await f.json()) as Record<string, string> };
      })();
      sampleCache.set(name, p);
    }
    return p;
  };

  const projectInput = (name: string): Promise<LoadInput> =>
    name === "__custom__" ? Promise.resolve(S.custom!.input) : fetchSample(name);

  // --- render --------------------------------------------------------------

  const lineageNote = (): string => {
    if (S.selectError) return "The selector is not valid.";
    if (S.spec.trim() === "") return "Type a selector, or pick one above.";
    if (S.matched.length === 0) return "No nodes match.";
    return "";
  };

  const projectLabel = (): string =>
    S.project === "__custom__" ? (S.custom?.fileName ?? "your manifest") : S.project;

  const renderSelection = () => {
    $("#pg-err")!.textContent = S.selectError;
    $("#pc-sel")!.textContent = String(S.matched.length);
    $("#pg-match-title")!.innerHTML = matchedTitle(S.total, S.matched.length);
    $("#pg-match")!.innerHTML = matchedHtml(S.matched, {
      spec: S.spec,
      error: S.selectError,
      was: S.prevHit,
      picked: S.picked,
    });
    $("#pg-lin-title")!.innerHTML = `lineage: <b>${esc(projectLabel())}</b>`;
    const note = lineageNote();
    if (S.lineage && !note) {
      const fresh = S.prevHit
        ? new Set([...S.hit.keys()].filter((id) => !S.prevHit!.has(id)))
        : null;
      const originX = Math.min(
        ...S.lineage.rects.filter((r) => S.hit.has(r.uid)).map((r) => r.x),
        0,
      );
      linHost.innerHTML = stageHtml(S.lineage, {
        hit: S.hit,
        picked: S.picked ? { uid: S.picked, name: S.pickedName } : null,
        fresh,
        originX,
      });
    } else {
      linHost.innerHTML = `<p class="pg-note">${esc(note)}</p>`;
    }
    $("#pg-cmd")!.textContent = lsCommand(S.spec);
  };

  const renderLint = () => {
    $("#pc-lint")!.textContent = String(lintCount(S.lint));
    $("#pg-lint-cmd")!.textContent = `$ ${lintCommand(S.spec)}`;
    $("#pg-lint")!.innerHTML = lintBodyHtml(S.lint, S.lintError);
    $("#pg-lint-foot")!.innerHTML = lintFootHtml(S.lint, {
      noExcerpts: S.project === "__custom__",
      extra: S.loadWarnings,
    });
    $("#pg-lcmd")!.textContent = lintCommand(S.spec);
    $("#pg-cfg-err")!.textContent = S.lintError.startsWith("invalid .dbtl.yml") ? S.lintError : "";
  };

  const sqlEl = $<HTMLTextAreaElement>("#pg-ed-sql")!;
  const cfgEl = $<HTMLTextAreaElement>("#pg-cfg")!;

  const renderDetail = () => {
    const d = S.detail;
    $("#pg-ed-name")!.textContent = d ? d.name : "—";
    $("#pg-ed-kind")!.textContent = d
      ? `${d.resource_type}${d.materialized ? ` · ${d.materialized}` : ""}${d.path ? ` · ${d.path}` : ""}`
      : "";
    const edited = !!S.picked && S.edited.has(S.picked);
    $("#pg-ed-badge")!.hidden = !edited;
    $<HTMLButtonElement>("#pg-ed-revert")!.disabled = !edited;
    $<HTMLButtonElement>("#pg-ed-cols")!.disabled = !d || d.columns.length === 0;
    if (d) {
      sqlEl.disabled = false;
      sqlEl.value = d.raw_code ?? "";
    } else {
      sqlEl.disabled = true;
      sqlEl.value = "";
    }
  };

  const colsHint = (): string => {
    if (S.colsError) return `error: ${S.colsError}`;
    if (!S.colNode) return "Pick a node and a column — or pick a node in the Select tab.";
    if (S.colCols.length === 0) return `${S.colNode} declares no columns.`;
    if (!S.colCol) return "Pick a column.";
    return "";
  };

  const renderCols = () => {
    $("#pg-cols")!.innerHTML = columnsBodyHtml(S.cols, colsHint());
    const name = S.nodesAll?.find((n) => n.unique_id === S.colNode)?.name ?? S.colNode ?? "";
    const target = S.colCol ? `${name}.${S.colCol}` : name;
    $("#pg-ccmd")!.textContent = columnsCommand(target, S.colUp);
    $<HTMLButtonElement>("#pg-col-dir")!
      .querySelectorAll("button")
      .forEach((b) => b.setAttribute("aria-pressed", String((b.dataset.d === "up") === S.colUp)));
  };

  const renderCfg = () => {
    const edited = cfgEl.value !== CONFIG_TEMPLATE;
    $("#pg-cfg-badge")!.hidden = !edited;
    $<HTMLButtonElement>("#pg-cfg-revert")!.disabled = !edited;
  };

  // --- engine jobs (each family guards itself with a generation counter) ---

  let seqSel = 0;
  let seqLint = 0;
  let seqDet = 0;
  let seqCols = 0;
  let seqLoad = 0;

  const refreshSelection = async () => {
    const my = ++seqSel;
    const spec = S.spec.trim();
    let nodes: SelectedNode[] = [];
    let error = "";
    let lineage: LineageOutput | null = null;
    if (spec !== "") {
      try {
        nodes = (await engine.select(spec)).nodes;
        if (nodes.length > 0) {
          lineage = await engine.lineage({
            roots: nodes.map((n) => n.unique_id),
            direction: "both",
            depth: 1,
            glyphs: "unicode",
            density: "comfortable",
          });
        }
      } catch (e) {
        if (transient(e) || my !== seqSel) return;
        error = `error: ${msg(e)} · exit 2`;
      }
    }
    if (my !== seqSel) return;
    S.prevHit = error || S.hit.size === 0 ? S.prevHit : new Set(S.hit.keys());
    S.matched = nodes;
    S.selectError = error;
    S.lineage = lineage;
    S.hit = new Map(nodes.map((n) => [n.unique_id, n.name] as const));
    renderSelection();
  };

  const refreshLint = async () => {
    const my = ++seqLint;
    try {
      const lint = await engine.lint({
        config: S.cfg,
        selector: S.spec.trim() || null,
      });
      if (my !== seqLint) return;
      S.lint = lint;
      S.lintError = "";
    } catch (e) {
      if (transient(e) || my !== seqLint) return;
      S.lint = null;
      S.lintError = msg(e);
    }
    renderLint();
  };

  let lintTimer = 0;
  const scheduleLint = (ms = 300) => {
    clearTimeout(lintTimer);
    lintTimer = window.setTimeout(() => void refreshLint(), ms);
  };

  const refreshAll = () => {
    void refreshSelection();
    scheduleLint(0);
    renderDetail();
    renderCols();
    renderCfg();
  };

  let connectBusy = false;
  let connectAgain = false;
  const connect = async () => {
    const my = ++seqLoad;
    if (connectBusy) {
      connectAgain = true;
      return;
    }
    connectBusy = true;
    try {
      const input = await projectInput(S.project);
      if (my !== seqLoad) return;
      const load = await engine.load(input);
      if (my !== seqLoad) return;
      S.total = load.nodes;
      S.loadWarnings = load.warnings;
      S.nodesAll = null;
      S.colNode = null;
      S.colCol = null;
      S.colCols = [];
      S.cols = null;
      S.picked = null;
      S.pickedName = "";
      S.detail = null;
      S.edited.clear();
      fillNodePicker();
      $("#pg-col-col")!.innerHTML = `<option value="">— pick a column —</option>`;
      refreshAll();
    } catch (e) {
      if (!transient(e)) {
        statusEl.textContent = `engine: failed — ${msg(e)}`;
      }
    } finally {
      connectBusy = false;
      if (connectAgain) {
        connectAgain = false;
        void connect();
      }
    }
  };

  // --- picking a node --------------------------------------------------------

  const pick = async (uid: string | null) => {
    const my = ++seqDet;
    S.picked = uid;
    if (uid === null) {
      S.detail = null;
      S.pickedName = "";
      renderDetail();
      renderSelection();
      return;
    }
    try {
      const d = await engine.nodeDetail(uid);
      if (my !== seqDet) return;
      S.detail = d;
      S.pickedName = d.name;
    } catch (e) {
      if (transient(e) || my !== seqDet) return;
      S.detail = null;
      S.pickedName = "";
      $("#pg-err")!.textContent = `error: ${msg(e)}`;
    }
    renderDetail();
    renderSelection();
    // Keep the columns picker aimed at the detail's node when it exists there.
    if (S.nodesAll?.some((n) => n.unique_id === uid) && S.colNode !== uid) {
      void selectColNode(uid);
    }
  };

  // --- columns ---------------------------------------------------------------

  const fillNodePicker = () => {
    $("#pg-col-node")!.innerHTML =
      `<option value="">— pick a node —</option>` + nodeOptionsHtml(S.nodesAll ?? [], S.colNode);
  };

  const ensureNodesAll = async () => {
    if (S.nodesAll) return;
    try {
      const all = (await engine.select("*")).nodes;
      S.nodesAll = all.filter((n) =>
        ["model", "seed", "snapshot", "source"].includes(n.resource_type),
      );
      fillNodePicker();
    } catch (e) {
      if (!transient(e)) $("#pg-err")!.textContent = `error: ${msg(e)}`;
    }
  };

  const selectColNode = async (uid: string) => {
    const my = ++seqCols;
    S.colNode = uid;
    const sel = $<HTMLSelectElement>("#pg-col-node")!;
    sel.value = uid;
    try {
      const d = await engine.nodeDetail(uid);
      if (my !== seqCols) return;
      S.colCols = d.columns;
      S.colCol = d.columns[0] ?? null;
      $("#pg-col-col")!.innerHTML = d.columns.length
        ? columnOptionsHtml(d.columns, S.colCol)
        : `<option value="">— no declared columns —</option>`;
      await refreshCols(my);
    } catch (e) {
      if (transient(e) || my !== seqCols) return;
      S.colCols = [];
      S.colCol = null;
      S.cols = null;
      S.colsError = msg(e);
      renderCols();
    }
  };

  const refreshCols = async (my = ++seqCols) => {
    if (!S.colNode || !S.colCol) {
      S.cols = null;
      S.colsError = "";
      renderCols();
      return;
    }
    try {
      const cols = await engine.columns({
        // unique_id, not the name — names can be ambiguous across kinds.
        target: `${S.colNode}.${S.colCol}`,
        upstream: S.colUp,
      });
      if (my !== seqCols) return;
      S.cols = cols;
      S.colsError = "";
    } catch (e) {
      if (transient(e) || my !== seqCols) return;
      S.cols = null;
      S.colsError = msg(e);
    }
    renderCols();
  };

  // --- events ----------------------------------------------------------------

  input.addEventListener("input", () => {
    S.spec = input.value;
    input.style.width = `${Math.max(1, S.spec.length)}ch`;
    void refreshSelection();
    scheduleLint();
  });

  $("#pg-eg")!.addEventListener("click", (ev) => {
    const b = (ev.target as Element).closest("button");
    if (!b) return;
    S.spec = b.textContent ?? "";
    input.value = S.spec;
    input.style.width = `${Math.max(1, S.spec.length)}ch`;
    void refreshSelection();
    scheduleLint();
  });

  // Project switching keeps the upload in memory: leaving the page drops it.
  const setProject = (name: string) => {
    if (name === S.project) return;
    if (name === "__custom__" && !S.custom) {
      $<HTMLInputElement>("#pg-file")!.click();
      return;
    }
    S.project = name;
    S.spec = EXAMPLES[name]?.[0] ?? "*";
    input.value = S.spec;
    input.style.width = `${Math.max(1, S.spec.length)}ch`;
    $("#pg-proj")!
      .querySelectorAll("button[data-p]")
      .forEach((b) =>
        b.setAttribute("aria-pressed", String((b as HTMLElement).dataset.p === name)),
      );
    $("#pg-eg")!.innerHTML = (EXAMPLES[name] ?? GENERIC_EXAMPLES)
      .map((e) => `<button type="button">${esc(e)}</button>`)
      .join("");
    void connect();
  };

  $("#pg-proj")!.addEventListener("click", (ev) => {
    const b = (ev.target as Element).closest<HTMLButtonElement>("button");
    if (b === $("#pg-pick")) {
      $<HTMLInputElement>("#pg-file")!.click();
      return;
    }
    if (b?.dataset.p) setProject(b.dataset.p);
  });

  // A local manifest.json: read into memory, parsed by the engine, shown as
  // "your manifest" — never uploaded, never persisted.
  const loadFile = async (f: File) => {
    const hint = $("#pg-file-hint")!;
    if (f.size > MAX_MANIFEST) {
      hint.textContent = `${f.name} is too large (max ${MAX_MANIFEST / 1024 / 1024} MiB)`;
      return;
    }
    hint.textContent = `loading ${f.name}…`;
    try {
      const input: LoadInput = { manifest: await f.text(), files: {} };
      const out = await engine.load(input); // a bad file throws; the old project stays loaded
      S.custom = { fileName: f.name, input, nodes: out.nodes };
      S.loadWarnings = out.warnings;
      const btn = $("#pg-custom")!;
      btn.textContent = f.name;
      btn.hidden = false;
      hint.textContent = `${f.name} — ${out.nodes} nodes, kept in memory only`;
      setProject("__custom__");
      $("#pg-proj")!
        .querySelectorAll("button[data-p]")
        .forEach((b) =>
          b.setAttribute("aria-pressed", String((b as HTMLElement).dataset.p === "__custom__")),
        );
    } catch (e) {
      hint.textContent = transient(e) ? "engine restarted — try again" : `${f.name}: ${msg(e)}`;
    }
  };

  $("#pg-file")!.addEventListener("change", (ev) => {
    const f = (ev.target as HTMLInputElement).files?.[0];
    if (f) void loadFile(f);
    (ev.target as HTMLInputElement).value = "";
  });
  ctl.addEventListener("dragover", (ev) => {
    ev.preventDefault();
    ctl.classList.add("over");
  });
  ctl.addEventListener("dragleave", () => ctl.classList.remove("over"));
  ctl.addEventListener("drop", (ev) => {
    ev.preventDefault();
    ctl.classList.remove("over");
    const f = ev.dataTransfer?.files?.[0];
    if (f) void loadFile(f);
  });

  $("#pg-match")!.addEventListener("click", (ev) => {
    const li = (ev.target as Element).closest<HTMLElement>("li[data-id]");
    if (li) void pick(li.dataset.id!);
  });

  // The engine lineage: hover lights a node's box, click opens its detail.
  linHost.addEventListener("click", (ev) => {
    const s = (ev.target as Element).closest("[data-n]");
    if (s) void pick(s.getAttribute("data-n"));
  });
  linHost.addEventListener("pointerover", (ev) => {
    const s = (ev.target as Element).closest("[data-n]");
    linHost.querySelectorAll(".hov").forEach((e) => e.classList.remove("hov"));
    if (!s) return;
    const q = s.getAttribute("data-n")!.replace(/["\\]/g, "\\$&");
    linHost.querySelectorAll(`[data-n="${q}"]`).forEach((e) => e.classList.add("hov"));
  });
  linHost.addEventListener("pointerleave", () =>
    linHost.querySelectorAll(".hov").forEach((e) => e.classList.remove("hov")),
  );

  // Editors: edits land in the engine ~300 ms after typing stops, then re-lint.
  let sqlTimer = 0;
  sqlEl.addEventListener("input", () => {
    if (!S.picked) return;
    clearTimeout(sqlTimer);
    sqlTimer = window.setTimeout(() => {
      const uid = S.picked;
      if (!uid) return;
      void (async () => {
        try {
          await engine.replaceSql(uid, sqlEl.value);
          S.edited.add(uid);
          $("#pg-ed-badge")!.hidden = false;
          $<HTMLButtonElement>("#pg-ed-revert")!.disabled = false;
          scheduleLint(0);
          if (S.colNode === uid) void refreshCols();
        } catch (e) {
          if (!transient(e)) $("#pg-err")!.textContent = `error: ${msg(e)}`;
        }
      })();
    }, 300);
  });
  $("#pg-ed-revert")!.addEventListener("click", () => {
    const uid = S.picked;
    if (!uid) return;
    void (async () => {
      try {
        await engine.revertSql(uid);
        S.edited.delete(uid);
        const d = await engine.nodeDetail(uid);
        S.detail = d;
        renderDetail();
        scheduleLint(0);
        if (S.colNode === uid) void refreshCols();
      } catch (e) {
        if (!transient(e)) $("#pg-err")!.textContent = `error: ${msg(e)}`;
      }
    })();
  });

  let cfgTimer = 0;
  cfgEl.addEventListener("input", () => {
    S.cfg = cfgEl.value;
    renderCfg();
    clearTimeout(cfgTimer);
    cfgTimer = window.setTimeout(() => void refreshLint(), 300);
  });
  $("#pg-cfg-revert")!.addEventListener("click", () => {
    cfgEl.value = CONFIG_TEMPLATE;
    S.cfg = CONFIG_TEMPLATE;
    renderCfg();
    void refreshLint();
  });

  // Columns tab controls.
  $<HTMLSelectElement>("#pg-col-node")!.addEventListener("change", (ev) => {
    const uid = (ev.target as HTMLSelectElement).value;
    if (uid) void selectColNode(uid);
  });
  $<HTMLSelectElement>("#pg-col-col")!.addEventListener("change", (ev) => {
    S.colCol = (ev.target as HTMLSelectElement).value || null;
    void refreshCols();
  });
  $("#pg-col-dir")!.addEventListener("click", (ev) => {
    const b = (ev.target as Element).closest("button");
    if (!b?.dataset.d) return;
    S.colUp = b.dataset.d === "up";
    void refreshCols();
  });
  $("#pg-ed-cols")!.addEventListener("click", () => {
    if (S.picked) {
      pickTab($("#pt-cols")!, false);
      void ensureNodesAll().then(() => {
        fillNodePicker();
        void selectColNode(S.picked!);
      });
    }
  });

  // Tabs.
  const tabs = $(".pg-tabs")!;
  const buttons = [...tabs.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
  const pickTab = (tab: HTMLButtonElement, focus: boolean) => {
    for (const b of buttons) {
      b.setAttribute("aria-selected", String(b === tab));
      b.tabIndex = b === tab ? 0 : -1;
    }
    if (focus) tab.focus();
    $("#pp-sel")!.hidden = tab.id !== "pt-sel";
    $("#pp-lint")!.hidden = tab.id !== "pt-lint";
    $("#pp-cols")!.hidden = tab.id !== "pt-cols";
    if (tab.id === "pt-cols") {
      void ensureNodesAll().then(() => {
        fillNodePicker();
        if (S.picked && !S.colNode) void selectColNode(S.picked);
      });
    }
  };
  buttons.forEach((tab, i) => {
    tab.addEventListener("click", () => pickTab(tab, false));
    tab.addEventListener("keydown", (ev) => {
      const n = buttons.length;
      const to =
        ev.key === "ArrowRight"
          ? (i + 1) % n
          : ev.key === "ArrowLeft"
            ? (i - 1 + n) % n
            : ev.key === "Home"
              ? 0
              : ev.key === "End"
                ? n - 1
                : -1;
      if (to < 0) return;
      ev.preventDefault();
      pickTab(buttons[to]!, true);
    });
  });
}
