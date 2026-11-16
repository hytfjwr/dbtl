//! The in-memory dbtl engine shared by the wasm bindings and native
//! callers: a manifest (plus the `.yml` texts lint positions resolve
//! against) loaded once, then selector, node-detail, lineage, lint, and
//! column-lineage operations whose output bytes match the CLI's.
//!
//! Everything here is pure — no filesystem, clock, randomness, or threads —
//! so the same code runs under `cargo test`, under Node, and inside a
//! Web Worker. Where the CLI prints or exits, the matching output or
//! error below carries the same wording, and the lint report goes through
//! `dbtl_report`'s assembly and renderers rather than a parallel path.

use dbtl_core::{
    self, closure_report, dag_from_manifest, layout_density, parse_manifest, selector_uses_state,
    validate_selector, CellAttr, ClosureDirection, ColumnRef, ColumnResolver, Dag, Edge, FailOn,
    MaterializationClass, NodeInfo, PackageScope, RawManifest, SqlDialect, SqlParseResolver,
    Subgraph,
};
use dbtl_report::{
    collect_sources, lint_report, render_report, LintAux, LintRunInputs, RenderCtx, RenderEnv,
    ReportFormat,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// How an engine failure maps onto the CLI's exit-code classes: `usage` is
/// the invocation's fault (a malformed selector, a bad `model.column`, an
/// inapplicable edit — exit 2) and `input` a bad or unreadable input (the
/// manifest text itself — exit 3). Lint reports its gate outcome as data —
/// [`LintOutput::exit_code`] — rather than as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The call could not be evaluated as asked (the CLI's exit 2 class).
    Usage,
    /// The named input could not be used (the CLI's exit 3 class).
    Input,
}

/// A failed engine operation: the CLI-flavored message plus the exit-code
/// class it would have exited with had it been a CLI error.
#[derive(Debug)]
pub struct EngineError {
    /// The exit-code class (`usage` = 2, `input` = 3).
    pub kind: ErrorKind,
    /// The message text — the same wording the CLI prints after `error:`.
    pub message: String,
}

impl EngineError {
    fn usage(message: impl Into<String>) -> Self {
        EngineError {
            kind: ErrorKind::Usage,
            message: message.into(),
        }
    }
    fn input(message: impl Into<String>) -> Self {
        EngineError {
            kind: ErrorKind::Input,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EngineError {}

/// The load call's input: the manifest text, the project-relative `.yml`
/// texts lint positions and excerpts resolve through, and the package
/// scope. `.yml` files only — `.sql` excerpts come from the manifest's own
/// `raw_code`, exactly like the CLI.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LoadInput {
    /// The `manifest.json` contents.
    pub manifest: String,
    /// `location.file`-relative `.yml` path → file text. Absent paths leave
    /// findings position-less, exactly like a missing file on disk.
    pub files: BTreeMap<String, String>,
    /// Keep installed-package nodes (`--include-packages`); the default
    /// prunes to the root project like every CLI command.
    pub include_packages: bool,
}

/// What `load` reports back: the project name, kept node counts by
/// resource type, and every warning line the CLI's load path would print.
#[derive(Debug, Serialize)]
pub struct LoadOutput {
    /// `metadata.project_name` (uid-segment fallback), `"dbt"` when none —
    /// the same label `dbtl lint` puts in its report header.
    pub project: String,
    /// Kept-node count per `resource_type`, sorted for determinism.
    pub counts: BTreeMap<String, usize>,
    /// Total kept nodes.
    pub nodes: usize,
    /// Loader warnings (unsupported/unknown resource types, the unused
    /// `functions` map), `(scope) `-prefixed package-scope warnings, then
    /// the unknown-adapter SQL-dialect notice — the same text the CLI's
    /// stderr sees, minus its `warning:` prefix.
    pub warnings: Vec<String>,
}

/// One selector-matched node — the columns `dbtl ls --format json` emits.
#[derive(Debug, Serialize)]
pub struct SelectedNode {
    /// `unique_id`.
    pub unique_id: String,
    /// The node's display name.
    pub name: String,
    /// `model` / `source` / `seed` / `snapshot` / `exposure` / …
    pub resource_type: String,
    /// The materialization (models/seeds/snapshots), else `null`.
    pub materialized: Option<String>,
    /// The project-relative file the node is declared in
    /// (`original_file_path`), when known.
    pub path: Option<String>,
}

/// `select`'s output.
#[derive(Debug, Serialize)]
pub struct SelectOutput {
    /// Matched nodes, sorted by `unique_id` — the same order
    /// `ls --output uids` prints.
    pub nodes: Vec<SelectedNode>,
    /// Evaluation notes — the `path:` legacy resource-relative form a
    /// spec still uses (the deprecation warning `ls` prints to stderr).
    pub warnings: Vec<String>,
}

/// The node-detail payload the SQL editor and column listing need.
#[derive(Debug, Serialize)]
pub struct DetailOutput {
    /// `unique_id`.
    pub unique_id: String,
    /// The node's display name.
    pub name: String,
    /// The resource type.
    pub resource_type: String,
    /// The materialization, when one is declared.
    pub materialized: Option<String>,
    /// The project-relative file the node is declared in
    /// (`original_file_path`), when known.
    pub path: Option<String>,
    /// The node's raw (uncompiled) SQL — the text `replace_sql` edits.
    pub raw_code: Option<String>,
    /// Whether a compiled SQL exists (pre-edit `compiled_code`).
    pub has_compiled_code: bool,
    /// Declared column names, in declaration order.
    pub columns: Vec<String>,
}

/// Which side(s) of a root the lineage view includes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Upstream and downstream — the full neighborhood.
    #[default]
    Both,
    /// Ancestors only.
    Upstream,
    /// Descendants only.
    Downstream,
}

/// The box-drawing repertoire — maps onto [`dbtl_core::GlyphMode`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlyphMode {
    /// `╭ ─ │ ▶` boxes/connectors.
    #[default]
    Unicode,
    /// `+ - | >` — unambiguous on every renderer.
    Ascii,
}

impl From<GlyphMode> for dbtl_core::GlyphMode {
    fn from(g: GlyphMode) -> Self {
        match g {
            GlyphMode::Unicode => Self::Unicode,
            GlyphMode::Ascii => Self::Ascii,
        }
    }
}

/// The node-box density — maps onto [`dbtl_core::Density`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// 3-row boxes with the materialization tag and `tests:N` borders.
    #[default]
    Comfortable,
    /// 1-row `│name│` nodes — the dense overview.
    Compact,
}

impl From<Density> for dbtl_core::Density {
    fn from(d: Density) -> Self {
        match d {
            Density::Comfortable => Self::Comfortable,
            Density::Compact => Self::Compact,
        }
    }
}

/// The lineage call's input: either `root` (one node's neighborhood) or
/// `roots` (the union of each root's neighborhood — a selector's match
/// set), plus the display options.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LineageInput {
    /// The node to center on — its directional closure, `depth`-limited.
    /// Wins over `roots` when both are present.
    pub root: Option<String>,
    /// A set of nodes whose neighborhoods merge into one diagram (each
    /// root's own directional closure). Unknown uids are ignored.
    pub roots: Vec<String>,
    /// Which side(s) of each root to include.
    pub direction: Direction,
    /// Hop limit per side (`null` = the whole closure).
    pub depth: Option<usize>,
    /// The box-drawing repertoire.
    pub glyphs: GlyphMode,
    /// The node-box density.
    pub density: Density,
}

/// One node's label rectangle on the grid.
#[derive(Debug, Serialize)]
pub struct LineageRect {
    /// `unique_id`.
    pub uid: String,
    /// Left cell x (inclusive).
    pub x: usize,
    /// Top cell y (inclusive).
    pub y: usize,
    /// Cells wide.
    pub w: usize,
    /// Cells tall.
    pub h: usize,
}

/// One dependency edge of the laid-out diagram — `parent` flows into
/// `child`. [`LineageOutput::cell_edges`] indexes into this list, which
/// keeps `Subgraph`'s edge order (sorted parent, then child).
#[derive(Debug, Serialize)]
pub struct LineageEdge {
    /// Parent node's `unique_id`.
    pub parent: String,
    /// Child node's `unique_id`.
    pub child: String,
}

/// The edges routed through one diagram cell, as indices into
/// [`LineageOutput::edges`]. A cell shared by several edges — a
/// child-side corner or an arrowhead — lists every one of them, the
/// completeness a hover or path highlighter needs. Derived by inverting
/// `Layout::edge_cells`' per-edge runs, with the arrow cell re-owned by
/// exactly the child's in-edges.
#[derive(Debug, Serialize)]
pub struct LineageCellEdges {
    /// Cell x.
    pub x: usize,
    /// Cell y.
    pub y: usize,
    /// Indices into `edges`, ascending.
    pub edges: Vec<usize>,
}

/// The laid-out lineage diagram: the character grid as row strings plus
/// parallel per-cell attribute grids, and the node label rectangles.
#[derive(Debug, Serialize)]
pub struct LineageOutput {
    /// Grid width in cells.
    pub width: usize,
    /// Grid height in cells.
    pub height: usize,
    /// One string per row, `width` chars each (trailing spaces significant).
    pub rows: Vec<String>,
    /// Per-cell materialization class, one char per cell: ` ` plain,
    /// `t` table, `v` view, `i` incremental, `e` ephemeral, `m` other
    /// model, `s` source, `d` seed, `n` snapshot, `x` exposure,
    /// `c` collapse marker.
    pub classes: Vec<String>,
    /// Per-cell emphasis flag (`'1'` = the selected node's label).
    pub emphasis: Vec<String>,
    /// Per-cell kind, one char per cell, parallel to `rows`: ` ` blank,
    /// `b` box border, `p` box padding, `n` node label, `x` test-count
    /// label, `e` connector, `a` arrowhead. Derived from `rects` — the
    /// spans a node box stamps — plus the routed connector cells, so a
    /// renderer can tell wire cells from node cells without re-running
    /// the layout.
    pub kinds: Vec<String>,
    /// The diagram's edges, sorted (parent, child) — the index space
    /// `cell_edges` refers into.
    pub edges: Vec<LineageEdge>,
    /// The edges routed through each connector cell, sorted by (y, x) —
    /// every edge entering a child is listed on that child's corner and
    /// arrow cells. Only connector-visible cells (kind `e`/`a`) appear:
    /// a run passing behind a box is trimmed to the cells `rows` shows.
    pub cell_edges: Vec<LineageCellEdges>,
    /// Node label rectangles, sorted by `unique_id`.
    pub rects: Vec<LineageRect>,
    /// The emphasized node's `unique_id`, or `null` when none.
    pub selected: Option<String>,
}

/// The lint call's input: the `.dbtl.yml` text (defaults when absent), an
/// optional narrowing selector, and the fail threshold.
///
/// There is no lint-baseline input — the run always behaves as if
/// `--no-baseline` were passed (a baseline file freezes findings from a
/// real project checkout, which the engine has no filesystem to read).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LintOptions {
    /// `.dbtl.yml` contents — parsed exactly like the on-disk file (the
    /// same unknown-key warnings apply). `null` = shipped defaults.
    pub config: Option<String>,
    /// A dbt selector narrowing findings to matched nodes — the
    /// `lint --select` flag's semantics, including its `invalid --select`
    /// error wording.
    pub selector: Option<String>,
    /// `error` (default) | `warn` | `never` — the `--fail-on` flag; the
    /// file's `fail_on:` fills in when absent.
    pub fail_on: Option<FailOn>,
    /// The `--show-suppressed` flag — suppressed findings list in the
    /// `text`/`ansi`/`json` outputs with their mechanism and reason (the
    /// summary counts them either way).
    pub show_suppressed: bool,
}

/// `dbtl lint`'s output as data: the three stdout bodies plus the exit
/// code and gate message the process would end with.
#[derive(Debug, Serialize)]
pub struct LintOutput {
    /// `dbtl lint --format text --color never` stdout, byte for byte.
    pub text: String,
    /// The same body with ANSI styling (`--color always`).
    pub ansi: String,
    /// `dbtl lint --format json` stdout, byte for byte.
    pub json: String,
    /// The process exit code — `0` pass, `1` gate fail.
    pub exit_code: i32,
    /// The gate failure line (`error: …` on stderr), when `exit_code != 0`.
    pub error: Option<String>,
    /// Non-fatal notes — `.dbtl.yml` warnings, an empty `--select` match.
    pub warnings: Vec<String>,
}

/// `dbtl columns`'s input.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ColumnsInput {
    /// `<model>.<column>` — `model` is a unique_id or a unique node name.
    pub target: String,
    /// List the column's provenance instead of its blast radius.
    pub upstream: bool,
}

/// `dbtl columns`'s output: the report in both shapes the CLI emits.
#[derive(Debug, Serialize)]
pub struct ColumnsOutput {
    /// `columns --format json` stdout, byte for byte.
    pub json: String,
    /// The default Markdown table output.
    pub markdown: String,
    /// The name-match fallback note the CLI writes to stderr, when any.
    pub warnings: Vec<String>,
}

/// `replace_sql` / `revert_sql` output.
#[derive(Debug, Serialize)]
pub struct ChangeOutput {
    /// Whether the engine's stored SQL changed.
    pub changed: bool,
}

/// A loaded manifest plus the inputs the operations evaluate against.
/// Build it once ([`Engine::load`]), then call the operations — every read
/// is deterministic and every result is the exact body the CLI would
/// print. `replace_sql` rewrites a model's SQL in place for re-linting;
/// `revert_sql` undoes it.
pub struct Engine {
    /// The manifest as parsed — `replace_sql` patches `raw_code` /
    /// `compiled_code` here and [`rebuild`](Self::rebuild) regenerates
    /// `dag`, so SQL edits flow through the same build path the load did.
    manifest: RawManifest,
    /// The scoped DAG the operations read (rebuilt after every SQL edit).
    dag: Dag,
    /// The package scope `load` applied — reapplied on every rebuild.
    scope: PackageScope,
    /// project-relative `.yml` texts for lint position/excerpt resolution.
    files: BTreeMap<String, String>,
    /// Load-time warnings — held only for the [`LoadOutput`] echo; SQL
    /// edits change no topology, so they stay valid across rebuilds.
    warnings: Vec<String>,
    /// `unique_id` → the manifest's own `(raw_code, compiled_code)` before
    /// the first edit — `revert_sql` restores from here.
    sql_edits: HashMap<String, (Option<String>, Option<String>)>,
}

impl Engine {
    /// Parse the manifest text, apply the package scope like the CLI's load
    /// path, and collect every warning the CLI would print — the loader's,
    /// then `(scope) `-prefixed scope warnings, then the unknown-adapter
    /// SQL-dialect notice.
    pub fn load(input: &LoadInput) -> Result<(Self, LoadOutput), EngineError> {
        // One parse, kept: the manifest stays behind for `replace_sql`
        // edits and `rebuild`, so `dag_from_manifest` (the same build +
        // warnings `dag_from_manifest_json` runs) avoids a second parse.
        let manifest =
            parse_manifest(&input.manifest).map_err(|e| EngineError::input(format!("{e:#}")))?;
        let (dag, mut warnings) = dag_from_manifest(&manifest);
        let scope = if input.include_packages {
            PackageScope::All
        } else {
            PackageScope::Root
        };
        let (dag, scope_warnings) = scope.apply_with_warnings(dag);
        warnings.extend(scope_warnings.iter().map(|w| format!("(scope) {w}")));
        if let Some(adapter) = SqlDialect::unrecognized_adapter(&dag) {
            warnings.push(format!(
                "column lineage: unknown adapter '{adapter}' — using the generic SQL dialect"
            ));
        }
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for n in dag.nodes().values() {
            *counts.entry(n.resource_type.clone()).or_insert(0) += 1;
        }
        let out = LoadOutput {
            project: dag.project_name().unwrap_or("dbt").to_string(),
            nodes: dag.len(),
            counts,
            warnings: warnings.clone(),
        };
        Ok((
            Engine {
                manifest,
                dag,
                scope,
                files: input.files.clone(),
                warnings,
                sql_edits: HashMap::new(),
            },
            out,
        ))
    }

    /// The scoped DAG the operations read.
    pub fn dag(&self) -> &Dag {
        &self.dag
    }

    /// The warnings `load` collected.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Evaluate a dbt selector against the loaded graph — the same
    /// strict-grammar validation and evaluation `dbtl ls` runs, so the
    /// uid set matches `ls <spec> --output uids` and the rows match
    /// `ls --format json`. A `state:` atom has no baseline to compare
    /// against here; it errors with the CLI's own wording.
    pub fn select(&self, spec: &str) -> Result<SelectOutput, EngineError> {
        self.select_labeled(spec, "selector")
    }

    /// [`select`](Self::select) with the CLI flag name the error message
    /// cites — `ls` reports `invalid selector '…'` while `--select`-carrying
    /// commands report `invalid --select '…'`.
    fn select_labeled(&self, spec: &str, flag: &str) -> Result<SelectOutput, EngineError> {
        validate_selector(spec)
            .map_err(|e| EngineError::usage(format!("invalid {flag} '{spec}': {e}")))?;
        if selector_uses_state(spec) {
            return Err(EngineError::usage(
                "a 'state:' selector needs --diff to name the baseline it compares against",
            ));
        }
        let (selected, warnings) = dbtl_core::select_report(&self.dag, None, spec);
        Ok(SelectOutput {
            nodes: selected
                .iter()
                .map(|uid| SelectedNode::new(&self.dag, uid))
                .collect(),
            warnings,
        })
    }

    /// The node-detail payload the SQL editor and column listing need:
    /// name, kind, materialization, file path, raw SQL, whether compiled
    /// SQL exists, and the declared column names in order.
    pub fn node_detail(&self, unique_id: &str) -> Result<DetailOutput, EngineError> {
        let Some(n) = self.dag.get(unique_id) else {
            return Err(EngineError::usage(format!("no node '{unique_id}'")));
        };
        let detail = self.dag.detail(unique_id);
        Ok(DetailOutput {
            unique_id: unique_id.to_string(),
            // `list_name` — the header shows `fct_orders.v1` for a
            // versioned model (same-name versions must not read
            // identically), the plain name elsewhere.
            name: dbtl_core::list_name(unique_id, &n.name),
            resource_type: n.resource_type.clone(),
            materialized: n.materialized.clone(),
            path: detail.and_then(|d| d.original_file_path.clone()),
            raw_code: self.dag.raw_code(unique_id).map(str::to_string),
            has_compiled_code: self.dag.compiled_code(unique_id).is_some(),
            columns: detail
                .map(|d| d.columns.iter().map(|c| c.name.clone()).collect())
                .unwrap_or_default(),
        })
    }

    /// The lineage diagram as a laid-out character grid — `dbtl-core`'s
    /// own placement (the same `Layout` the TUI draws), exported as row
    /// strings plus parallel per-cell class/emphasis grids and the node
    /// label rectangles. Rendering is the caller's job; placement is the
    /// engine's, so a terminal and a browser can never disagree.
    pub fn lineage(&self, input: &LineageInput) -> Result<LineageOutput, EngineError> {
        let (upstream, downstream) = match input.direction {
            Direction::Both => (true, true),
            Direction::Upstream => (true, false),
            Direction::Downstream => (false, true),
        };
        let sg = match &input.root {
            Some(root) => {
                if !self.dag.contains(root) {
                    return Err(EngineError::usage(format!("no node '{root}'")));
                }
                self.dag
                    .subgraph_view(root, upstream, downstream, input.depth)
            }
            None => self.union_subgraph(&input.roots, upstream, downstream, input.depth),
        };
        let density = input.density.into();
        let mut lay = layout_density(&sg, input.glyphs.into(), density);
        // Stamp each node's materialization class onto its label cells —
        // the same classify() the TUI's style pass uses.
        let styles: BTreeMap<String, CellAttr> = sg
            .nodes
            .iter()
            .map(|n| {
                (
                    n.unique_id.clone(),
                    CellAttr {
                        class: MaterializationClass::classify(
                            &n.resource_type,
                            n.materialized.as_deref(),
                        ),
                        ..CellAttr::default()
                    },
                )
            })
            .collect();
        lay.apply_node_styles(&styles);
        let grid = &lay.grid;
        let (width, height) = (grid.width(), grid.height());
        let rows = (0..height).map(|y| grid.row_string(y)).collect();
        let classes = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| class_code(grid.attr_at(x, y).class))
                    .collect()
            })
            .collect();
        let emphasis = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| if grid.emphasis_at(x, y) { '1' } else { '0' })
                    .collect::<String>()
            })
            .collect();
        let mut rects: Vec<LineageRect> = lay
            .rects
            .iter()
            .map(|(uid, r)| LineageRect {
                uid: uid.clone(),
                x: r.x,
                y: r.y,
                w: r.width,
                h: r.height,
            })
            .collect();
        rects.sort_by(|a, b| a.uid.cmp(&b.uid));
        let (kinds, edges, cell_edges) = lineage_cells(&lay, &sg, density);
        Ok(LineageOutput {
            width,
            height,
            rows,
            classes,
            emphasis,
            kinds,
            edges,
            cell_edges,
            rects,
            selected: (!sg.selected.is_empty()).then(|| sg.selected.clone()),
        })
    }

    /// `dbtl lint` in memory: the same `dbtl_report` assembly and
    /// renderers the CLI drives — `text`/`ansi`/`json` are byte-identical
    /// to `lint --format text --color never|always` and
    /// `lint --format json`, and `exit_code`/`error` carry the gate
    /// verdict the process would exit with. No catalog is ever loaded, so
    /// the warehouse-dependent rules skip exactly like a catalog-less CLI
    /// run.
    pub fn lint(&self, input: &LintOptions) -> Result<LintOutput, EngineError> {
        let config = match &input.config {
            Some(text) => dbtl_core::ProjectConfig::from_yaml(text).map_err(EngineError::usage)?,
            None => dbtl_core::ProjectConfig::default(),
        };
        let mut warnings = config.warnings.clone();
        let fail_on = input.fail_on.or(config.fail_on).unwrap_or(FailOn::Error);
        let selection = match &input.selector {
            Some(spec) => {
                let selected = self.select_labeled(spec, "--select")?;
                warnings.extend(selected.warnings.iter().cloned());
                let set: BTreeSet<String> =
                    selected.nodes.iter().map(|n| n.unique_id.clone()).collect();
                if set.is_empty() {
                    warnings.push(format!("--select '{spec}' matched no nodes"));
                }
                Some(set)
            }
            None => None,
        };
        let resolver = SqlParseResolver::for_dag(None, &self.dag);
        let read_text = |file: &str| self.files.get(file).cloned();
        let run = lint_report(
            self.dag.project_name().unwrap_or("dbt"),
            self.scope.describe(),
            &LintRunInputs {
                dag: &self.dag,
                cfg: &config.lint,
                base: None,
                baseline: None,
                fail_on,
                aux: LintAux {
                    sql: Some(&resolver),
                    catalog: None,
                    base_sql: None,
                },
                selection: selection.as_ref(),
                show_suppressed: input.show_suppressed,
                read_text: &read_text,
            },
        );
        // `.dbtl.yml` `exclude:`/selector-param deprecation notes — the same
        // non-fatal-warning channel the config-load warnings ride. A spec
        // shared between `--select` and an `exclude:` notes once.
        warnings.extend(run.warnings.iter().cloned());
        let mut seen = BTreeSet::new();
        warnings.retain(|w| seen.insert(w.clone()));
        let env = RenderEnv {
            dag: &self.dag,
            read_text: &read_text,
        };
        let sources = collect_sources(&run.report, &env);
        let ctx = RenderCtx {
            command: "lint",
            path_prefix: None,
            dag: Some(&self.dag),
            sources: Some(&sources),
            styled: false,
        };
        let text = render_report(ReportFormat::Text, &run.report, &ctx);
        let ansi = render_report(
            ReportFormat::Text,
            &run.report,
            &RenderCtx {
                styled: true,
                ..ctx
            },
        );
        let json = render_report(ReportFormat::Json, &run.report, &ctx);
        let (exit_code, error) = if run.failed {
            (
                1,
                Some(format!(
                    "lint: {} finding(s) at or above fail-on '{}'",
                    run.gating,
                    run.fail_on.name()
                )),
            )
        } else {
            (0, None)
        };
        Ok(LintOutput {
            text,
            ansi,
            json,
            exit_code,
            error,
            warnings,
        })
    }

    /// `dbtl columns <model>.<column>` in memory: the same name/uid
    /// resolution, known-column check, and `closure_report`. No warehouse
    /// catalog is ever loaded, so columns resolve parser-then-name-match
    /// like a catalog-less CLI run.
    pub fn columns(&self, input: &ColumnsInput) -> Result<ColumnsOutput, EngineError> {
        let (model, column) = input
            .target
            .rsplit_once('.')
            .filter(|(m, c)| !m.is_empty() && !c.is_empty())
            .ok_or_else(|| {
                EngineError::usage(format!(
                    "columns: expected <model>.<column>, got '{}'",
                    input.target
                ))
            })?;
        let uid = self.model_uid(model)?;
        let resolver = SqlParseResolver::for_dag(None, &self.dag);
        let col = ColumnRef::new(&uid, column);
        let known = resolver.columns(&self.dag, &uid);
        if !known.contains(&col.column) {
            return Err(EngineError::usage(format!(
                "columns: '{model}' has no column '{column}' (known: {})",
                if known.is_empty() {
                    "(none — the node has no resolvable columns)".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
        let direction = if input.upstream {
            ClosureDirection::Upstream
        } else {
            ClosureDirection::Downstream
        };
        let report = closure_report(&self.dag, &resolver, &col, direction);
        let json = format!(
            "{}\n",
            serde_json::to_string_pretty(&report).expect("the column report serializes")
        );
        let markdown = report.to_string();
        let fallbacks = resolver.fallback_count();
        let warnings = if fallbacks > 0 {
            vec![format!(
                "columns: {fallbacks} model{} resolved by name-match fallback (missing/unparsable SQL)",
                if fallbacks == 1 { "" } else { "s" }
            )]
        } else {
            Vec::new()
        };
        Ok(ColumnsOutput {
            json,
            markdown,
            warnings,
        })
    }

    /// Replace a model/snapshot's raw SQL in memory and drop its compiled
    /// SQL — column lineage resolves `compiled_code` before `raw_code`, so
    /// a stale compiled text must go with the edit or resolution would keep
    /// reading the pre-edit SQL. The DAG is rebuilt from the patched
    /// manifest, so lint and lineage see the same edited graph. Only nodes
    /// in the scoped graph are editable — a package node the scope pruned
    /// is invisible to every read, so silently rewriting its SQL would
    /// leave state nothing observes.
    pub fn replace_sql(&mut self, unique_id: &str, sql: &str) -> Result<ChangeOutput, EngineError> {
        if !self.dag.contains(unique_id) {
            return Err(EngineError::usage(format!("no node '{unique_id}'")));
        }
        let Some(node) = self.manifest.nodes.get(unique_id) else {
            // Sources/exposures are graph nodes but live outside
            // `manifest.nodes` — never SQL-editable.
            let kind = self
                .dag
                .get(unique_id)
                .map(|n| n.resource_type.as_str())
                .unwrap_or("node");
            return Err(EngineError::usage(format!(
                "'{unique_id}' is a {kind}, not a model or snapshot"
            )));
        };
        if !matches!(node.resource_type.as_str(), "model" | "snapshot") {
            return Err(EngineError::usage(format!(
                "'{unique_id}' is a {}, not a model or snapshot",
                node.resource_type
            )));
        }
        self.sql_edits
            .entry(unique_id.to_string())
            .or_insert_with(|| (node.raw_code.clone(), node.compiled_code.clone()));
        let node = self
            .manifest
            .nodes
            .get_mut(unique_id)
            .expect("checked above");
        node.raw_code = Some(sql.to_string());
        node.compiled_code = None;
        self.rebuild();
        Ok(ChangeOutput { changed: true })
    }

    /// Restore a node's SQL to its loaded state — the `replace_sql` revert.
    /// `changed` is `false` when the node was never edited; an out-of-scope
    /// or unknown `unique_id` is the same usage error `replace_sql` gives.
    pub fn revert_sql(&mut self, unique_id: &str) -> Result<ChangeOutput, EngineError> {
        if !self.dag.contains(unique_id) {
            return Err(EngineError::usage(format!("no node '{unique_id}'")));
        }
        let Some((raw, compiled)) = self.sql_edits.remove(unique_id) else {
            return Ok(ChangeOutput { changed: false });
        };
        if let Some(node) = self.manifest.nodes.get_mut(unique_id) {
            node.raw_code = raw;
            node.compiled_code = compiled;
            self.rebuild();
        }
        Ok(ChangeOutput { changed: true })
    }

    /// The `<model>` half of `dbtl columns`'s `<model>.<column>`: an exact
    /// unique_id, else the single node of that name — the CLI's own
    /// wording for unknown and ambiguous names. The `list_name` spelling
    /// matches too: `fct_orders.v1` addresses that one version while
    /// `fct_orders` still reports the family as ambiguous.
    fn model_uid(&self, model: &str) -> Result<String, EngineError> {
        if self.dag.contains(model) {
            return Ok(model.to_string());
        }
        let mut hits: Vec<&str> = self
            .dag
            .nodes()
            .values()
            .filter(|n| n.name == model || dbtl_core::list_name(&n.unique_id, &n.name) == model)
            .map(|n| n.unique_id.as_str())
            .collect();
        hits.sort();
        match hits.as_slice() {
            [one] => Ok((*one).to_string()),
            [] => Err(EngineError::usage(format!(
                "columns: no node named '{model}'"
            ))),
            _ => Err(EngineError::usage(format!(
                "columns: '{model}' is ambiguous ({}); pass the unique_id",
                hits.join(", ")
            ))),
        }
    }

    /// The combined neighborhood of several roots — each root's own
    /// directional closure merged into one subgraph (shared nodes/edges
    /// deduplicated, ordering deterministic).
    fn union_subgraph(
        &self,
        roots: &[String],
        upstream: bool,
        downstream: bool,
        depth: Option<usize>,
    ) -> Subgraph {
        let mut nodes: BTreeMap<String, NodeInfo> = BTreeMap::new();
        let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
        let mut selected = String::new();
        for root in roots {
            if !self.dag.contains(root) {
                continue;
            }
            if selected.is_empty() {
                selected = root.clone();
            }
            let sg = self.dag.subgraph_view(root, upstream, downstream, depth);
            for n in sg.nodes {
                nodes.entry(n.unique_id.clone()).or_insert(n);
            }
            for e in sg.edges {
                edges.insert((e.parent, e.child));
            }
        }
        Subgraph {
            selected,
            nodes: nodes.into_values().collect(),
            edges: edges
                .into_iter()
                .map(|(parent, child)| Edge { parent, child })
                .collect(),
        }
    }

    /// Regenerate `dag` from the (possibly edited) manifest — the same
    /// build and package-scope application `load` ran, so an edit can
    /// never leave the graph and the manifest disagreeing.
    fn rebuild(&mut self) {
        self.dag = self.scope.apply_with_warnings(Dag::build(&self.manifest)).0;
    }
}

impl SelectedNode {
    fn new(dag: &Dag, uid: &str) -> Self {
        let n = dag.get(uid);
        SelectedNode {
            unique_id: uid.to_string(),
            name: n.map(|n| n.name.clone()).unwrap_or_default(),
            resource_type: n.map(|n| n.resource_type.clone()).unwrap_or_default(),
            materialized: n.and_then(|n| n.materialized.clone()),
            path: dag.detail(uid).and_then(|d| d.original_file_path.clone()),
        }
    }
}

/// The per-cell materialization-class code in [`LineageOutput::classes`]
/// rows — one char per `MaterializationClass`, `' '` for unstyled cells
/// (connectors, padding). Codes are arbitrary but stable; the JS renderer
/// maps them to colors.
fn class_code(class: MaterializationClass) -> char {
    match class {
        MaterializationClass::Plain => ' ',
        MaterializationClass::Table => 't',
        MaterializationClass::View => 'v',
        MaterializationClass::Incremental => 'i',
        MaterializationClass::Ephemeral => 'e',
        MaterializationClass::OtherModel => 'm',
        MaterializationClass::Source => 's',
        MaterializationClass::Seed => 'd',
        MaterializationClass::Snapshot => 'n',
        MaterializationClass::Exposure => 'x',
        MaterializationClass::Collapsed => 'c',
    }
}

/// The cell-kind and edge-ownership derivation behind
/// [`LineageOutput::kinds`], `edges`, and `cell_edges` — a pure re-read
/// of the `Layout` + `Subgraph`, never a re-draw.
///
/// Kinds come first from the routed connector cells (every recorded run
/// cell is `e`, each child-side arrowhead cell is `a`), then the node
/// boxes stamp over them: a rect's connector-attach row is side borders
/// plus label and padding, its remaining rows are border, and the
/// `tests:N` run on the bottom border is its own kind. Those are exactly
/// the spans `draw_box`/`draw_compact_box` write, so the kinds agree
/// with `rows` wherever they overlap.
///
/// `cell_edges` inverts `Layout::edge_cells` into per-cell lists. That
/// map is edge → cells, and the cells a shared corner or the arrowhead
/// visually serves are already claimed by every edge that needs them —
/// inverting therefore yields the multi-edge ownership a highlighter
/// wants. One correction remains: the arrow cell `(child.x - 1,
/// child.y + box_mid)` can also sit inside a crossing edge's run (the
/// degenerate channel that falls back onto the arrow column), so it is
/// re-owned by exactly the child's in-edge set — matching how the arrow
/// glyph wins the cell in the painter's second pass. Only cells whose
/// kind ended up `e` or `a` are listed, so a run that passes behind a
/// box reports only its visible cells.
///
/// The kind stamping below mirrors the classic box shapes
/// (`draw_box`/`draw_compact_box`); `lineage()` only ever lays out
/// through `layout_density`, so those are the only rect shapes to read.
fn lineage_cells(
    lay: &dbtl_core::Layout,
    sg: &Subgraph,
    density: dbtl_core::Density,
) -> (Vec<String>, Vec<LineageEdge>, Vec<LineageCellEdges>) {
    let box_mid = match density {
        dbtl_core::Density::Comfortable => 1,
        dbtl_core::Density::Compact => 0,
    };
    let (width, height) = (lay.grid.width(), lay.grid.height());
    let mut kinds = vec![vec![b' '; width]; height];

    // Per-cell edge claimants: indices into `edges` (the sorted Subgraph
    // edge list), collected in that same ascending order.
    let mut claimed: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (i, e) in sg.edges.iter().enumerate() {
        if let Some(run) = lay.edge_cells.get(&(e.parent.clone(), e.child.clone())) {
            for &(x, y) in run {
                claimed.entry((x, y)).or_default().push(i);
            }
        }
    }
    // Arrowhead cells belong to every edge entering the child (see the
    // doc above); insert() overwrites any run claimant of the cell.
    let mut arrows: Vec<(usize, usize)> = Vec::new();
    for node in &sg.nodes {
        let Some(r) = lay.rects.get(&node.unique_id) else {
            continue;
        };
        if r.x == 0 {
            continue;
        }
        let ins: Vec<usize> = sg
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.child == node.unique_id)
            .map(|(i, _)| i)
            .collect();
        if !ins.is_empty() {
            let cell = (r.x - 1, r.y + box_mid);
            claimed.insert(cell, ins);
            arrows.push(cell);
        }
    }
    for &(x, y) in claimed.keys() {
        kinds[y][x] = b'e';
    }
    for &(x, y) in &arrows {
        kinds[y][x] = b'a';
    }

    // Node boxes stamp over connectors — the same cells `draw_box` /
    // `draw_compact_box` paint, classified by what was written there.
    let info: HashMap<&str, &NodeInfo> =
        sg.nodes.iter().map(|n| (n.unique_id.as_str(), n)).collect();
    for (uid, r) in &lay.rects {
        let Some(n) = info.get(uid.as_str()) else {
            continue;
        };
        let (bx, by, w, h) = (r.x, r.y, r.width, r.height);
        for (yy, row) in kinds
            .iter_mut()
            .enumerate()
            .take((by + h).min(height))
            .skip(by)
        {
            for (xx, cell) in row
                .iter_mut()
                .enumerate()
                .take((bx + w).min(width))
                .skip(bx)
            {
                *cell = if yy == by + box_mid {
                    if xx == bx || xx == bx + w - 1 {
                        b'b'
                    } else {
                        b'p'
                    }
                } else {
                    b'b'
                };
            }
        }
        // The label span on the attach row: `bx + 1 + box_mid + i` while
        // it still fits before the right border — the span both box
        // painters write (and the cells `emphasis` marks).
        let label_len = dbtl_core::list_name(uid, &n.name).chars().count();
        let lx = bx + 1 + box_mid;
        for i in 0..label_len {
            if lx + i < bx + w - 1 && lx + i < width && by + box_mid < height {
                kinds[by + box_mid][lx + i] = b'n';
            }
        }
        // `tests:N` right-aligned on the bottom border — the bordered
        // density only.
        if box_mid == 1 && n.test_count > 0 && by + h <= height {
            let tlen = format!("tests:{}", n.test_count).chars().count();
            let start = (bx + w - 1).saturating_sub(1 + tlen);
            for i in 0..tlen {
                if start + i > bx && start + i < width {
                    kinds[by + h - 1][start + i] = b'x';
                }
            }
        }
    }

    let mut cell_edges: Vec<LineageCellEdges> = claimed
        .into_iter()
        .filter(|&((x, y), _)| matches!(kinds[y][x], b'e' | b'a'))
        .map(|((x, y), mut idxs)| {
            idxs.dedup();
            LineageCellEdges { x, y, edges: idxs }
        })
        .collect();
    cell_edges.sort_by_key(|c| (c.y, c.x));
    let edges = sg
        .edges
        .iter()
        .map(|e| LineageEdge {
            parent: e.parent.clone(),
            child: e.child.clone(),
        })
        .collect();
    (
        kinds
            .into_iter()
            .map(|row| String::from_utf8(row).expect("kind codes are ASCII"))
            .collect(),
        edges,
        cell_edges,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn load_fixture() -> (Engine, LoadOutput) {
        Engine::load(&LoadInput {
            manifest: fixture("manifest.json"),
            ..LoadInput::default()
        })
        .expect("fixture loads")
    }

    #[test]
    fn load_reports_the_project_and_counts() {
        let (_, out) = load_fixture();
        assert_eq!(out.project, "jaffle_finance");
        assert!(out.counts["model"] > 0);
        assert_eq!(out.nodes, out.counts.values().sum::<usize>());
        // The fixture's adapter is known — no dialect warning.
        assert!(out.warnings.iter().all(|w| !w.contains("unknown adapter")));
    }

    #[test]
    fn load_rejects_broken_json_with_the_cli_wording() {
        let err = Engine::load(&LoadInput {
            manifest: "not json".to_string(),
            ..LoadInput::default()
        })
        .err()
        .expect("broken JSON is an error");
        assert_eq!(err.kind, ErrorKind::Input);
        assert!(
            err.message.starts_with("failed to parse manifest JSON: "),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn select_returns_the_matched_uids_sorted() {
        let (engine, _) = load_fixture();
        let out = engine.select("tag:marts").unwrap();
        assert!(!out.nodes.is_empty());
        let uids: Vec<&str> = out.nodes.iter().map(|n| n.unique_id.as_str()).collect();
        let mut sorted = uids.clone();
        sorted.sort();
        assert_eq!(uids, sorted, "nodes come back uid-sorted");
        assert!(uids.iter().all(|u| u.starts_with("model.")));
    }

    #[test]
    fn select_errors_like_the_cli() {
        let (engine, _) = load_fixture();
        let err = engine.select("bad:spec:nop").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
        assert!(
            err.message.starts_with("invalid selector 'bad:spec:nop': "),
            "message: {}",
            err.message
        );
        let err = engine.select("state:modified").unwrap_err();
        assert_eq!(
            err.message,
            "a 'state:' selector needs --diff to name the baseline it compares against"
        );
    }

    #[test]
    fn lineage_rows_match_the_tui_grid() {
        let (engine, _) = load_fixture();
        let out = engine
            .lineage(&LineageInput {
                root: Some("model.jaffle_finance.fct_subscription_process".to_string()),
                ..LineageInput::default()
            })
            .unwrap();
        // The same layout the TUI would draw for that node's subgraph.
        let sg = engine
            .dag()
            .subgraph("model.jaffle_finance.fct_subscription_process");
        let want = dbtl_core::layout_mode(&sg, dbtl_core::GlyphMode::Unicode)
            .grid
            .to_text();
        assert_eq!(out.rows.join("\n"), want);
        assert!(out
            .rects
            .iter()
            .any(|r| r.uid == "model.jaffle_finance.fct_subscription_process"));
        // Emphasis marks exactly the selected node's label cells.
        assert_eq!(
            out.selected.as_deref(),
            Some("model.jaffle_finance.fct_subscription_process")
        );
    }

    #[test]
    fn lineage_cells_describe_the_diagram_per_cell() {
        let (engine, _) = load_fixture();
        let root = "model.jaffle_finance.fct_subscription_process";
        let out = engine
            .lineage(&LineageInput {
                root: Some(root.to_string()),
                ..LineageInput::default()
            })
            .unwrap();
        let sg = engine.dag().subgraph(root);
        let lay = dbtl_core::layout_mode(&sg, dbtl_core::GlyphMode::Unicode);

        // kinds is parallel to rows, and a cell is blank-kind only when the
        // glyph there is blank.
        assert_eq!(out.kinds.len(), out.height);
        for y in 0..out.height {
            assert_eq!(out.kinds[y].chars().count(), out.width);
            for (x, ch) in out.rows[y].chars().enumerate() {
                let k = out.kinds[y].as_bytes()[x];
                assert!(
                    k != b' ' || ch == ' ',
                    "({x},{y}) has glyph {ch:?} but kind ' '"
                );
            }
        }

        // A box-kind cell sits inside exactly one rect; a connector cell
        // sits inside none.
        let inside = |x: usize, y: usize| {
            out.rects
                .iter()
                .filter(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
                .count()
        };
        for (y, row) in out.kinds.iter().enumerate() {
            for (x, k) in row.bytes().enumerate() {
                match k {
                    b'b' | b'p' | b'n' | b'x' => assert_eq!(inside(x, y), 1, "({x},{y}) kind {k}"),
                    b'e' | b'a' => assert_eq!(inside(x, y), 0, "({x},{y}) kind {k}"),
                    b' ' => {}
                    _ => panic!("({x},{y}) unknown kind {k}"),
                }
            }
        }

        // cell_edges is (y, x)-sorted, names only connector-visible cells,
        // and lists ascending in-range edge indices. The edges list itself
        // keeps the sorted (parent, child) order the index math assumes.
        let keys: Vec<(&str, &str)> = out
            .edges
            .iter()
            .map(|e| (e.parent.as_str(), e.child.as_str()))
            .collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]));
        let mut prev = (0, 0);
        for (i, ce) in out.cell_edges.iter().enumerate() {
            if i > 0 {
                assert!((ce.y, ce.x) > prev, "cell_edges sorted by (y, x)");
            }
            prev = (ce.y, ce.x);
            assert!(matches!(out.kinds[ce.y].as_bytes()[ce.x], b'e' | b'a'));
            assert!(!ce.edges.is_empty());
            assert!(ce.edges.windows(2).all(|w| w[0] < w[1]));
            assert!(ce.edges.iter().all(|&e| e < out.edges.len()));
        }

        // The arrowhead cell of each child with in-edges lists exactly those
        // edges — every entering edge, not just the one whose run drew it.
        for node in &sg.nodes {
            let ins: Vec<usize> = sg
                .edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.child == node.unique_id)
                .map(|(i, _)| i)
                .collect();
            if ins.is_empty() {
                continue;
            }
            let r = &lay.rects[&node.unique_id];
            let cell = (r.x - 1, r.y + 1);
            if out.kinds[cell.1].as_bytes()[cell.0] != b'a' {
                continue; // the cell sits under a box — nothing visible to claim
            }
            let ce = out
                .cell_edges
                .iter()
                .find(|ce| (ce.x, ce.y) == cell)
                .expect("arrow cell is listed");
            assert_eq!(ce.edges, ins, "arrow cell of {}", node.unique_id);
        }

        // Every cell core recorded for an edge either still lists it (kind
        // 'e') or was won by something later — a box stamping over the run,
        // or an arrowhead whose in-edges replaced the run's earlier claim.
        for (i, e) in sg.edges.iter().enumerate() {
            let Some(run) = lay.edge_cells.get(&(e.parent.clone(), e.child.clone())) else {
                continue;
            };
            for &(x, y) in run {
                if out.kinds[y].as_bytes()[x] == b'e' {
                    let ce = out
                        .cell_edges
                        .iter()
                        .find(|ce| ce.x == x && ce.y == y)
                        .unwrap();
                    assert!(
                        ce.edges.contains(&i),
                        "({x},{y}) is a visible cell of edge {i} {:?} but does not list it",
                        (e.parent.as_str(), e.child.as_str())
                    );
                }
            }
        }

        // The output serializes deterministically — same input, same bytes.
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            serde_json::to_string(
                &engine
                    .lineage(&LineageInput {
                        root: Some(root.to_string()),
                        ..LineageInput::default()
                    })
                    .unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn lint_reports_all_three_bodies_and_the_gate() {
        let (engine, _) = load_fixture();
        let out = engine.lint(&LintOptions::default()).unwrap();
        assert_eq!(out.exit_code, 1, "the fixture has error findings");
        assert!(out.text.contains("error["), "text: {}", &out.text[..200]);
        assert!(out.ansi.contains('\u{1b}'), "the styled body carries ANSI");
        let json: serde_json::Value = serde_json::from_str(&out.json).unwrap();
        assert_eq!(json["schema"], "dbtl.report/v1");
        assert_eq!(json["summary"]["errors"], 2);
        assert_eq!(
            out.error.as_deref(),
            Some("lint: 2 finding(s) at or above fail-on 'error'")
        );
    }

    #[test]
    fn lint_honors_a_custom_config() {
        let (engine, _) = load_fixture();
        let out = engine
            .lint(&LintOptions {
                config: Some("fail_on: never\n".to_string()),
                ..LintOptions::default()
            })
            .unwrap();
        assert_eq!(out.exit_code, 0, "fail_on never never gates");
        assert!(out.error.is_none());
        let err = engine
            .lint(&LintOptions {
                config: Some("lint:\n  rules:\n    no_such_rule: true\n".to_string()),
                ..LintOptions::default()
            })
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
        assert!(err.message.contains("unknown lint rule 'no_such_rule'"));
    }

    #[test]
    fn lint_show_suppressed_lists_the_suppressed_findings() {
        let (engine, _) = load_fixture();
        // `unused_models` fires exactly once on the fixture — a named
        // exclude spec suppresses it with a reason.
        let config = "lint:\n  rules:\n    unused_models:\n      exclude: [{select: \"int_shoppers__combined\", reason: kept for a downstream spike}]\n"
            .to_string();
        // Without the flag the suppressed findings count in the summary
        // but never list.
        let out = engine
            .lint(&LintOptions {
                config: Some(config.clone()),
                ..LintOptions::default()
            })
            .unwrap();
        assert!(
            out.text.contains("suppressed") && !out.text.contains("kept for a downstream spike"),
            "the count reports, the reasons stay hidden:\n{}",
            out.text
        );
        let json: serde_json::Value = serde_json::from_str(&out.json).unwrap();
        let total = json["summary"]["suppressed"]["total"]
            .as_u64()
            .expect("summary.suppressed.total");
        assert_eq!(total, 1, "the fixture's unused_models finding suppressed");
        // With the flag they list — marked with mechanism + reason — in
        // both bodies.
        let out = engine
            .lint(&LintOptions {
                config: Some(config),
                show_suppressed: true,
                ..LintOptions::default()
            })
            .unwrap();
        assert!(
            out.text
                .contains("suppressed: exclude — kept for a downstream spike"),
            "the marker names mechanism + reason:\n{}",
            out.text
        );
        let json: serde_json::Value = serde_json::from_str(&out.json).unwrap();
        let suppressed: Vec<&serde_json::Value> = json["gates"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| {
                g["findings"]
                    .as_array()
                    .map(|a| a.iter())
                    .unwrap_or_default()
            })
            .filter(|f| f["suppressed"] == true)
            .collect();
        assert_eq!(suppressed.len() as u64, total, "every one lists");
        for f in &suppressed {
            assert_eq!(f["suppression"]["by"], "exclude", "{f}");
            assert_eq!(
                f["suppression"]["reason"], "kept for a downstream spike",
                "{f}"
            );
        }
    }

    #[test]
    fn columns_reports_downstream() {
        let (engine, _) = load_fixture();
        let out = engine
            .columns(&ColumnsInput {
                target: "fct_subscription_process.subscription_id".to_string(),
                upstream: false,
            })
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&out.json).unwrap();
        assert_eq!(json["target"], "fct_subscription_process.subscription_id");
        assert!(out.markdown.contains("# Column lineage:"));
        let err = engine
            .columns(&ColumnsInput {
                target: "fct_subscription_process.no_such_col".to_string(),
                upstream: false,
            })
            .unwrap_err();
        assert!(err.message.contains("has no column 'no_such_col'"));
    }

    #[test]
    fn replace_and_revert_sql_roundtrip() {
        let (mut engine, _) = load_fixture();
        let uid = "model.jaffle_finance.fct_subscription_process";
        let before = engine.node_detail(uid).unwrap();
        assert!(before.raw_code.is_some());
        engine.replace_sql(uid, "select 1 as id").unwrap();
        let after = engine.node_detail(uid).unwrap();
        assert_eq!(after.raw_code.as_deref(), Some("select 1 as id"));
        assert!(
            !after.has_compiled_code,
            "the edit drops the stale compiled SQL"
        );
        let changed = engine.revert_sql(uid).unwrap();
        assert!(changed.changed);
        let restored = engine.node_detail(uid).unwrap();
        assert_eq!(restored.raw_code, before.raw_code);
        assert_eq!(restored.has_compiled_code, before.has_compiled_code);
        // A second revert is a no-op.
        assert!(!engine.revert_sql(uid).unwrap().changed);
        // Non-model nodes refuse the edit — a source names what it is,
        // not "no node" (it IS a graph node, just not an editable one).
        let err = engine
            .replace_sql(
                "source.jaffle_finance.prod_lake_jaffle_payment.suppliers",
                "x",
            )
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
        assert!(err.message.contains("is a source"), "message: {err}");
    }

    #[test]
    fn select_nodes_carry_the_ls_json_path() {
        let (engine, _) = load_fixture();
        let out = engine.select("fct_subscription_process").unwrap();
        let node = out
            .nodes
            .iter()
            .find(|n| n.unique_id == "model.jaffle_finance.fct_subscription_process")
            .expect("the model is selected");
        assert_eq!(
            node.path.as_deref(),
            Some(
                "models/marts/dimensional/jaffle/subscription_process/fct_subscription_process.sql"
            ),
            "path mirrors ls --format json's original_file_path"
        );
    }

    #[test]
    fn replace_and_revert_refuse_out_of_scope_nodes() {
        // The packages fixture keeps installed-package nodes out of the
        // root-scoped graph: they load (they're in the raw manifest) but
        // are invisible to every operation — edits must refuse them too.
        let (mut engine, _) = Engine::load(&LoadInput {
            manifest: fixture("packages_manifest.json"),
            ..LoadInput::default()
        })
        .expect("packages fixture loads");
        let pruned = "model.audit_tools.audit_events";
        assert!(
            engine.manifest.nodes.contains_key(pruned),
            "the uid exists in the raw manifest but not the scoped dag"
        );
        assert!(!engine.dag().contains(pruned));
        let err = engine.replace_sql(pruned, "select 1").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
        assert_eq!(err.message, format!("no node '{pruned}'"));
        assert_eq!(
            engine.revert_sql(pruned).unwrap_err().message,
            format!("no node '{pruned}'")
        );
        // include_packages keeps the node in scope — then it IS editable.
        let (mut engine, _) = Engine::load(&LoadInput {
            manifest: fixture("packages_manifest.json"),
            include_packages: true,
            ..LoadInput::default()
        })
        .expect("packages fixture loads with packages");
        assert!(engine.dag().contains(pruned));
        if engine
            .node_detail(pruned)
            .map(|d| d.resource_type == "model")
            .unwrap_or(false)
        {
            assert!(engine.replace_sql(pruned, "select 1").is_ok());
        }
    }

    #[test]
    fn node_detail_returns_the_editor_payload() {
        let (engine, _) = load_fixture();
        let d = engine
            .node_detail("model.jaffle_finance.fct_subscription_process")
            .unwrap();
        assert_eq!(d.unique_id, "model.jaffle_finance.fct_subscription_process");
        assert_eq!(d.name, "fct_subscription_process");
        assert_eq!(d.resource_type, "model");
        assert_eq!(d.materialized.as_deref(), Some("table"));
        assert_eq!(
            d.path.as_deref(),
            Some(
                "models/marts/dimensional/jaffle/subscription_process/fct_subscription_process.sql"
            )
        );
        assert!(
            d.raw_code
                .as_deref()
                .is_some_and(|s| s.contains("fct_subscription_process")),
            "raw_code: {:?}",
            d.raw_code
        );
        // The fixture carries raw_code only — no compiled artifacts.
        assert!(!d.has_compiled_code);
        assert_eq!(
            d.columns,
            [
                "subscription_id",
                "delivery_id",
                "delivery_lane_key",
                "supplier_key",
                "supplier_department_key",
                "fiscal_year_key",
                "received_date",
            ]
            .map(String::from)
        );

        // A source keeps its kind but has no editable SQL.
        let src = engine
            .node_detail("source.jaffle_finance.dev_lake_jaffle_finance.budgets")
            .unwrap();
        assert_eq!(src.resource_type, "source");
        assert!(src.raw_code.is_none());
        assert!(!src.has_compiled_code);

        let err = engine.node_detail("model.jaffle_finance.nope").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
        assert_eq!(err.message, "no node 'model.jaffle_finance.nope'");
    }

    #[test]
    fn node_detail_names_versioned_models_uniquely() {
        // The same-name `fct_orders` versions must not read identically —
        // `list_name` appends the `.vN` uid segment.
        let (engine, _) = Engine::load(&LoadInput {
            manifest: fixture("versioned_manifest.json"),
            ..LoadInput::default()
        })
        .expect("versioned fixture loads");
        let v1 = engine.node_detail("model.versioned.fct_orders.v1").unwrap();
        assert_eq!(v1.name, "fct_orders.v1");
        assert_eq!(v1.resource_type, "model");
        assert_eq!(v1.materialized.as_deref(), Some("view"));
        assert_eq!(v1.path.as_deref(), Some("models/fct_orders_v1.sql"));
        assert!(v1.has_compiled_code, "the fixture carries compiled_code");
        let vbeta = engine
            .node_detail("model.versioned.fct_orders.vbeta")
            .unwrap();
        assert_eq!(vbeta.name, "fct_orders.vbeta");
        // A non-versioned model keeps its plain name.
        let use_v1 = engine.node_detail("model.versioned.use_v1").unwrap();
        assert_eq!(use_v1.name, "use_v1");
    }

    #[test]
    fn warnings_echo_the_load_output() {
        // `warnings()` is the load's vec — same lines, same order.
        let (engine, out) = load_fixture();
        assert_eq!(engine.warnings(), out.warnings.as_slice());

        // An adapter the SQL dialect table doesn't know names itself.
        let mut m: serde_json::Value =
            serde_json::from_str(&fixture("sample_manifest.json")).unwrap();
        m["metadata"]["adapter_type"] = serde_json::json!("weirddb");
        let (engine, out) = Engine::load(&LoadInput {
            manifest: m.to_string(),
            ..LoadInput::default()
        })
        .expect("patched manifest loads");
        let dialect = "column lineage: unknown adapter 'weirddb' — using the generic SQL dialect";
        assert!(
            out.warnings.iter().any(|w| w == dialect),
            "warnings: {:#?}",
            out.warnings
        );
        assert_eq!(engine.warnings(), out.warnings.as_slice());

        // Several packages + no recorded project_name → the root-scope
        // guess is warned, `(scope) `-prefixed.
        let mut m: serde_json::Value =
            serde_json::from_str(&fixture("packages_manifest.json")).unwrap();
        m["metadata"]["project_name"] = serde_json::Value::Null;
        let (engine, out) = Engine::load(&LoadInput {
            manifest: m.to_string(),
            ..LoadInput::default()
        })
        .expect("patched manifest loads");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.starts_with("(scope) ") && w.contains("metadata.project_name")),
            "warnings: {:#?}",
            out.warnings
        );
        assert_eq!(engine.warnings(), out.warnings.as_slice());
    }
}
