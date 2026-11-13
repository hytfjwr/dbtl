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
    self, closure_report, dag_from_manifest_json, layout_density, parse_manifest,
    selector_uses_state, validate_selector, CellAttr, ClosureDirection, ColumnRef, ColumnResolver,
    Dag, Edge, FailOn, MaterializationClass, NodeInfo, PackageScope, RawManifest, SqlDialect,
    SqlParseResolver, Subgraph,
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
}

/// `select`'s output.
#[derive(Debug, Serialize)]
pub struct SelectOutput {
    /// Matched nodes, sorted by `unique_id` — the same order
    /// `ls --output uids` prints.
    pub nodes: Vec<SelectedNode>,
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
    /// Node label rectangles, sorted by `unique_id`.
    pub rects: Vec<LineageRect>,
    /// The emphasized node's `unique_id`, or `null` when none.
    pub selected: Option<String>,
}

/// The lint call's input: the `.dbtl.yml` text (defaults when absent), an
/// optional narrowing selector, and the fail threshold.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LintOptions {
    /// `.dbtl.yml` contents — parsed exactly like the on-disk file (the
    /// same unknown-key warnings apply). `null` = shipped defaults.
    pub config: Option<String>,
    /// A dbt selector narrowing findings to matched nodes — the
    /// `lint --select` flag's semantics.
    pub selector: Option<String>,
    /// `error` (default) | `warn` | `never` — the `--fail-on` flag; the
    /// file's `fail_on:` fills in when absent.
    pub fail_on: Option<FailOn>,
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
        let (dag, mut warnings) = dag_from_manifest_json(&input.manifest)
            .map_err(|e| EngineError::input(format!("{e:#}")))?;
        let manifest =
            parse_manifest(&input.manifest).map_err(|e| EngineError::input(format!("{e:#}")))?;
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
    /// uid set matches `ls <spec> --output uids`. A `state:` atom has no
    /// baseline to compare against here; it errors with the CLI's own
    /// wording.
    pub fn select(&self, spec: &str) -> Result<SelectOutput, EngineError> {
        validate_selector(spec)
            .map_err(|e| EngineError::usage(format!("invalid selector '{spec}': {e}")))?;
        if selector_uses_state(spec) {
            return Err(EngineError::usage(
                "a 'state:' selector needs --diff to name the baseline it compares against",
            ));
        }
        let selected = dbtl_core::select(&self.dag, None, spec);
        Ok(SelectOutput {
            nodes: selected
                .iter()
                .map(|uid| SelectedNode::new(uid, self.dag.get(uid)))
                .collect(),
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
            name: n.name.clone(),
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
        let mut lay = layout_density(&sg, input.glyphs.into(), input.density.into());
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
        Ok(LineageOutput {
            width,
            height,
            rows,
            classes,
            emphasis,
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
                let selected = self.select(spec)?;
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
                read_text: &read_text,
            },
        );
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
    /// manifest, so lint and lineage see the same edited graph.
    pub fn replace_sql(&mut self, unique_id: &str, sql: &str) -> Result<ChangeOutput, EngineError> {
        let Some(node) = self.manifest.nodes.get(unique_id) else {
            return Err(EngineError::usage(format!("no node '{unique_id}'")));
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
    /// `changed` is `false` when the node was never edited.
    pub fn revert_sql(&mut self, unique_id: &str) -> Result<ChangeOutput, EngineError> {
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
    /// wording for unknown and ambiguous names.
    fn model_uid(&self, model: &str) -> Result<String, EngineError> {
        if self.dag.contains(model) {
            return Ok(model.to_string());
        }
        let mut hits: Vec<&str> = self
            .dag
            .nodes()
            .values()
            .filter(|n| n.name == model)
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
    fn new(uid: &str, n: Option<&NodeInfo>) -> Self {
        SelectedNode {
            unique_id: uid.to_string(),
            name: n.map(|n| n.name.clone()).unwrap_or_default(),
            resource_type: n.map(|n| n.resource_type.clone()).unwrap_or_default(),
            materialized: n.and_then(|n| n.materialized.clone()),
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
        // Non-model nodes refuse the edit.
        let err = engine
            .replace_sql(
                "source.jaffle_finance.prod_lake_jaffle_payment.suppliers",
                "x",
            )
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
    }
}
