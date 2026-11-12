//! The pruned dependency DAG and its value types: merge a parsed manifest's
//! `nodes` (excluding `test` / `operation` / `analysis` / `check`), `sources`,
//! and `exposures` into a single `unique_id -> NodeInfo` map, build pruned
//! adjacency over the kept set, and compute upstream / downstream transitive
//! closures (cycle-safe).
//! Side maps (`details` / `tests` / `sql` / `compiled` / `exposures`) are
//! captured pre-prune as data only — they never touch the topology.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::compiled::{CompiledOrigin, CompiledSql};
use crate::manifest::{
    load_manifest, RawConstraint, RawDoc, RawExposure, RawFreshnessThreshold, RawMacro,
    RawManifest, RawNode, RawSource,
};

/// A merged, prefix-resolved entry in the unified node map.
///
/// `materialized` and `test_count` are the only details kept on `NodeInfo`
/// (rest live on [`NodeDetail`]) because the lineage *layout* needs them to
/// render each node's box: the materialization tag in the top border and the
/// `tests:N` label in the bottom border. `materialized` is `None` for sources
/// (no materialization) and for models with none recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeInfo {
    pub unique_id: String,
    pub name: String,
    pub resource_type: String,
    pub path: Option<String>,
    pub materialized: Option<String>,
    /// Count of direct (1-hop) parents / children in the pruned DAG. Surfaced as
    /// list-pane badges; `direct_down == 0` flags a leaf ("orphan") model.
    pub direct_up: usize,
    pub direct_down: usize,
    /// Number of tests attached to this node (the size of its `Dag::tests`
    /// entry, captured pre-prune). Carried here so the lineage layout can show
    /// a `tests:N` label without reaching back into the `Dag`.
    pub test_count: usize,
}

/// A node that SHOULD have tests but has none: a buildable resource
/// (model / snapshot / seed) with `test_count == 0`. Sources are excluded
/// (covered at the source-freshness layer, not via dbt tests here). Pure over
/// [`NodeInfo`]'s `resource_type` + `test_count`, both of which the manifest↔source
/// consistency suite asserts identical across load modes — so it is the single
/// source of truth for the coverage lens, the list tint, and the coverage %.
pub fn coverage_gap(n: &NodeInfo) -> bool {
    matches!(n.resource_type.as_str(), "model" | "snapshot" | "seed") && n.test_count == 0
}

/// Whether a description string carries no real content — the whitespace-only
/// case (`" "`, `"\n  "`) that every "is it documented?" judgment must treat
/// as missing (absence itself is `Option`'s job). The single shared predicate
/// behind `clean_desc` (model descriptions), the column-description filter in
/// `columns_from` (both load modes), and the doc-coverage gate's
/// `column_documented`.
pub fn description_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Whether a node is a declared downstream consumer (an exposure). Pure over
/// [`NodeInfo::resource_type`] — the single source of truth for the exposure
/// predicate (c.f. [`coverage_gap`]), so the impact split and the report can
/// never disagree on what counts as one.
pub fn is_exposure(n: &NodeInfo) -> bool {
    n.resource_type == "exposure"
}

/// Which packages a loaded [`Dag`] is presented in — the CLI `--include-packages`
/// flag's domain meaning, shared by the TUI (`App` carries it so `reload`
/// re-applies it), `dbtl docs`/`check`/`impact`, and the [`DiffReport`](crate::DiffReport)
/// payload (which serializes it as `"root"` / `"all"`).
///
/// - `Root` (the default): only the root project's own nodes — the package
///   boundary [`Dag::is_root_project`] defines. Installed-package models
///   (`dbt deps` / `packages.yml`) are pruned at load, which also makes
///   manifest mode match source mode (which never parses `dbt_packages/`).
/// - `All` (`--include-packages`): the manifest's full contents — package
///   nodes stay in the graph as dependency context for the list, lineage, and
///   docs pages. Aggregate metrics (coverage / orphans / stats / docs summary
///   / `dbtl check` / the PR-impact risk flags) still count the root project
///   only: installed-package nodes are context, never the counted surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageScope {
    /// The root project only — installed-package nodes are pruned at load.
    #[default]
    Root,
    /// All packages the manifest carries; metrics still count the root project.
    All,
}

impl PackageScope {
    /// Whether installed-package nodes are kept in the graph (`--include-packages`).
    pub fn includes_packages(&self) -> bool {
        matches!(self, Self::All)
    }

    /// The compact status-bar token after `scope:` (`"root"` / `"+pkgs"`).
    /// Pure ASCII, so it is safe under [`crate`]'s ascii-guard contract.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::All => "+pkgs",
        }
    }

    /// The sentence-form label for docs / CLI output: the summary's `- scope:`
    /// bullet, the `dbtl check` report line, and the impact report's header.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Root => "root project only",
            Self::All => "all packages (metrics count the root project only)",
        }
    }

    /// Apply the scope to a just-loaded `Dag`: `Root` prunes to the root
    /// project's own nodes ([`Dag::root_scoped`]); `All` keeps the graph whole.
    /// No-ops cheaply when there is nothing to prune (source mode and
    /// single-package manifests are already root-only). Discards the warnings
    /// [`apply_with_warnings`](PackageScope::apply_with_warnings) reports —
    /// callers that surface scope outcomes (the CLI load path, `App::reload`)
    /// use that instead.
    pub fn apply(&self, dag: Dag) -> Dag {
        self.apply_with_warnings(dag).0
    }

    /// [`apply`](PackageScope::apply) plus the warnings the scope decision
    /// produced — RETURNED, not printed, so every caller surfaces them its own
    /// way (the CLI prints them on stderr; `App::reload` merges them into the
    /// warnings vec its toast quotes). Two warnings exist, both about trusting
    /// the outcome too much:
    ///
    /// - the AMBIGUOUS-ROOT guess: the manifest records no
    ///   `metadata.project_name` (pre-dbt-1.6 or hand-built) AND more than one
    ///   package is present, so [`Dag::primary_package`]'s frequency fallback
    ///   picked the root — a guess that inverts when an installed package
    ///   outnumbers the project. Emitted under EITHER scope: `All` skips the
    ///   prune but every root-scoped metric still trusts the same guess.
    /// - the EMPTIED graph: the `Root` scope pruned a non-empty graph to
    ///   nothing (the recorded/inferred root contributes no kept nodes — a
    ///   deps-only project, or the guess above gone wrong). The empty graph is
    ///   honest; silently re-scoping to a foreign package would show the exact
    ///   opposite of the scope's intent.
    pub fn apply_with_warnings(&self, dag: Dag) -> (Dag, Vec<String>) {
        let mut warnings = Vec::new();
        // The owning package resolved ONCE, shared by the ambiguity check, the
        // already-root-only early-out, and the prune predicate — the
        // unrecorded fallback is an O(nodes) scan, so nothing below may
        // re-resolve it (the shape [`Dag::root_scoped`] already documents).
        let primary = dag.primary_package();
        if dag.recorded_project().is_none() && dag.package_histogram().len() > 1 {
            warnings.push(format!(
                "the manifest records no metadata.project_name (pre-dbt-1.6 or \
                 hand-built) and multiple packages are present — the root \
                 project was inferred as '{}' by node frequency, a guess an \
                 installed package can steal when it outnumbers the project \
                 (recompile with dbt >=1.6 to pin it)",
                primary.as_deref().unwrap_or("<unresolved>")
            ));
        }
        match self {
            Self::All => (dag, warnings),
            Self::Root => {
                let keep = |n: &NodeInfo| primary.as_deref().is_none_or(|pkg| in_package(n, pkg));
                if dag.nodes().values().all(&keep) {
                    (dag, warnings) // already root-only — skip the prune + clone
                } else {
                    // The severed-adjacency record: for each SURVIVING node,
                    // how many direct parents/children the prune removes
                    // because they live in an installed package. `filtered`
                    // drops those neighbours — without this map, rules that
                    // judge connectedness (`root_models`, `unused_sources`)
                    // would read a package-trimmed node as isolated. Counted
                    // here (inside the scope decision) only: a plain
                    // `filtered` — `docs --select` narrowing — is a
                    // presentation cut, not a package boundary.
                    let mut severed: HashMap<String, PrunedAdjacent> = HashMap::new();
                    for (uid, n) in dag.nodes() {
                        if !keep(n) {
                            continue;
                        }
                        let parents = dag
                            .direct_parents(uid)
                            .iter()
                            .filter(|p| dag.get(p).is_none_or(|m| !keep(m)))
                            .count();
                        let children = dag
                            .direct_children(uid)
                            .iter()
                            .filter(|c| dag.get(c).is_none_or(|m| !keep(m)))
                            .count();
                        if parents + children > 0 {
                            severed.insert(uid.clone(), PrunedAdjacent { parents, children });
                        }
                    }
                    let mut scoped = dag.filtered(keep);
                    for (uid, s) in severed {
                        let e = scoped.pruned_adjacent.entry(uid).or_default();
                        e.parents += s.parents;
                        e.children += s.children;
                    }
                    if scoped.is_empty() {
                        if let Some(pkg) = &primary {
                            warnings.push(format!(
                                "no nodes belong to the root project '{pkg}' — \
                                 the graph is empty (--include-packages keeps \
                                 installed-package nodes in scope)"
                            ));
                        }
                    }
                    (scoped, warnings)
                }
            }
        }
    }
}

/// One column of a model/source, as recorded in the manifest. Kept in dbt
/// *definition* order (the manifest preserves it once `serde_json`'s
/// `preserve_order` feature is on).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: Option<String>,
    pub description: Option<String>,
    /// The column-level `constraints:` (a column's constraint covers itself —
    /// `columns` stays empty on the domain side; the wire omits the key).
    pub constraints: Vec<ConstraintInfo>,
}

/// A `constraints:` entry, carried verbatim minus dbt's adapter bookkeeping
/// (`warn_unenforced`/`warn_unsupported`). Model-level constraints hang off
/// [`NodeDetail::constraints`]; column-level off [`ColumnInfo::constraints`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConstraintInfo {
    /// `primary_key` / `not_null` / `foreign_key` / `unique` / `check` /
    /// `custom` — empty when dbt recorded no `type` (it always does).
    pub constraint_type: String,
    /// The constraint's `name:` override (mostly used by `check`).
    pub name: Option<String>,
    /// The `check` (or `expression`-carrying) constraint's SQL text.
    pub expression: Option<String>,
    /// A `foreign_key` constraint's target (`ref('x')`/`source('a','b')`
    /// expression or a bare model name).
    pub to: Option<String>,
    /// The referenced columns for `foreign_key`.
    pub to_columns: Vec<String>,
    /// The model columns a model-level constraint covers (always empty for
    /// column-level constraints).
    pub columns: Vec<String>,
}

/// A test attached to a node, captured *pre-prune* (tests are excluded from the
/// DAG, so this is the only place they survive). `kind` is the generic test
/// name (`unique`, `not_null`, `relationships`, …) or `"singular"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestInfo {
    pub name: String,
    pub kind: String,
    pub column_name: Option<String>,
    /// A `relationships` test's referenced node uid (the `to:` target), resolved
    /// from `depends_on` minus the attached node. `None` for other kinds, for a
    /// `to:` dbt can't resolve, or when the manifest records no depends_on pair.
    /// The node may still have been pruned out of the graph (a dropped source)
    /// — check `ref_name`/the Dag before navigating.
    pub ref_uid: Option<String>,
    /// `ref_uid`'s display name, resolved post-build once every node exists —
    /// so the structure modal and the docs table never re-resolve. `None` when
    /// the target node isn't in the kept graph.
    pub ref_name: Option<String>,
    /// A `relationships` test's referenced column (the kwargs `field:`).
    pub ref_column: Option<String>,
    /// The unit-test shape (dbt ≥1.8), present only on `kind == "unit"`.
    pub unit: Option<UnitTestShape>,
}

/// The manifest-carried shape of a dbt unit test: how many `given:` input
/// tables feed it and how many rows each side declares. Row counts are
/// `Some` only where the manifest carries an inline `rows` list — a `fixture:`
/// reference or a CSV-string `rows` payload is `None` ("where the manifest
/// carries them", per the contract).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitTestShape {
    /// Number of `given:` input tables the test feeds the model.
    pub inputs: usize,
    /// Total inline `rows:` across the `given:` inputs — `Some` only when
    /// EVERY input declares an inline `rows` list (a partial count would
    /// understate the real input size).
    pub given_rows: Option<usize>,
    /// The `expect:` inline `rows:` count, same inline-list caveat.
    pub expect_rows: Option<usize>,
}

/// Whether a node's [`TestInfo`] set guarantees a primary key — the SHARED
/// predicate the `missing_primary_key_tests` lint rule and the `PrimaryKey`
/// lineage lens quote, so the two can never disagree:
/// a `unique` + `not_null` pair on the SAME column, or any
/// `unique_combination_of_columns` test (the `ends_with` keeps the bare kind
/// plus package-namespaced spellings like `dbt_utils.unique_combination_of_columns`).
pub fn has_pk_guarantee(tests: &[TestInfo]) -> bool {
    let mut uniques: BTreeSet<&str> = BTreeSet::new();
    let mut not_nulls: BTreeSet<&str> = BTreeSet::new();
    for t in tests {
        match t.kind.as_str() {
            "unique" => {
                if let Some(c) = &t.column_name {
                    uniques.insert(c.as_str());
                }
            }
            "not_null" => {
                if let Some(c) = &t.column_name {
                    not_nulls.insert(c.as_str());
                }
            }
            kind if kind.ends_with("unique_combination_of_columns") => return true,
            _ => {}
        }
    }
    !uniques.is_disjoint(&not_nulls)
}

/// The exposure-specific payload (kind / owner / url), hung off the [`Dag`] as
/// a side map like `details`/`tests` — so `NodeInfo` (the hot subgraph
/// clone/equality path) and `NodeDetail` (frozen literals) stay untouched.
/// Consumed by the impact report's "Affected exposures" section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposureInfo {
    /// `dashboard` / `notebook` / `analysis` / `ml` / `application`, when recorded.
    pub exposure_type: Option<String>,
    pub owner_name: Option<String>,
    pub owner_email: Option<String>,
    pub url: Option<String>,
}

/// The adjacency a [`PackageScope::Root`] prune severed for a kept node —
/// how many of its direct parents/children disappeared because they belong
/// to an installed package. Hung off the [`Dag`] as a side map (like
/// `details`/`tests`): it is a LOAD-SCOPE record, not node identity, so
/// `NodeInfo` (the hot clone/equality path) and the manifest↔source
/// consistency contract stay untouched — and source mode never fills it
/// (it never parsed the packages). `root_models`/`unused_sources` read it
/// so a node whose only neighbours were package-side isn't reported as
/// isolated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrunedAdjacent {
    /// Direct parents the scope prune removed.
    pub parents: usize,
    /// Direct children the scope prune removed.
    pub children: usize,
}

/// One source freshness threshold (`{count: 12, period: hour}`). A threshold
/// whose count dbt left null carries no signal — it never reaches this type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FreshnessThreshold {
    pub count: i64,
    pub period: Option<String>,
}

/// The detail payload for a node, used by the structure modal and the status
/// line. Hung off the [`Dag`] (NOT [`NodeInfo`]) so the hot subgraph clone /
/// equality path stays cheap and the frozen `NodeInfo` literals are untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeDetail {
    pub materialized: Option<String>,
    /// The node's effective `unique_key`, normalized to a list regardless of
    /// the declared scalar/list shape (empty = unconfigured). Comes from
    /// `config(unique_key=…)` > schema.yml `config:` > the project tree's
    /// `+unique_key` — whichever the loader's input declared.
    pub unique_key: Vec<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub columns: Vec<ColumnInfo>,
    pub original_file_path: Option<String>,
    /// The `.yml` properties file that patches this node (manifest
    /// `patch_path` with its `<package>://` scheme stripped so it matches the
    /// `original_file_path` project-relative convention; `None` when no YAML
    /// entry declares the node — location code then falls back to
    /// `original_file_path`).
    pub patch_path: Option<String>,
    /// The dbt package that defined the node (manifest `package_name`) —
    /// distinguishes installed-package models from the project's own. `None`
    /// in source mode (where every node is local).
    pub package_name: Option<String>,
    // --- governance (dbt model access controls) ---
    /// `private`/`protected`/`public` — who may `ref()` this model.
    pub access: Option<String>,
    /// The `groups:` entry the model belongs to.
    pub group: Option<String>,
    /// `contract.enforced` — declared columns are the enforced schema.
    pub contract_enforced: bool,
    // --- model configuration (the rest of the governance-adjacent fields) ---
    /// `config.incremental_strategy` — `append` / `merge` / `delete+insert` /
    /// `insert_overwrite` / `microbatch`; `None` when undeclared (dbt falls
    /// back to the adapter default).
    pub incremental_strategy: Option<String>,
    /// `deprecation_date` — when the model stops being supported, kept as the
    /// recorded string (manifest mode carries dbt's ISO timestamp, source
    /// mode the declared date text — the consistency suite compares the
    /// date portion).
    pub deprecation_date: Option<String>,
    /// The node's `language` — `"sql"` for everything source mode collects
    /// (it only scans .sql), `"python"` for python models in a manifest.
    /// `None` on seeds / non-buildable nodes.
    pub language: Option<String>,
    /// The model-level `constraints:` list (column-level constraints live on
    /// [`ColumnInfo::constraints`]).
    pub constraints: Vec<ConstraintInfo>,
    /// This node's version name, when the model is versioned.
    pub version: Option<String>,
    /// The newest version's name, when the model is versioned.
    pub latest_version: Option<String>,
    /// The node's `meta` dictionary as stringified key-value pairs (owner /
    /// domain / arbitrary annotations): scalar values unquoted, nested
    /// objects/arrays as compact JSON. `BTreeMap` so every surface (modal,
    /// docs, selectors, grouping) iterates in sorted-key order.
    pub meta: BTreeMap<String, String>,
    /// The node's effective warehouse relation name when it DIFFERS from
    /// `name` — `alias` for models/snapshots/seeds, `identifier` for source
    /// tables (`Some` only on a real override). CLL matches compiled-SQL
    /// `FROM "db"."schema"."<relation>"` against it; `None` means the
    /// `name`/`unique_id` segment is the relation.
    pub relation_name: Option<String>,
    // --- source ingestion metadata (sources only) ---
    /// `loaded_at_field` — the timestamp column freshness is measured
    /// against. The already-resolved effective value (table > source).
    pub loaded_at_field: Option<String>,
    /// `loader:` — the tool that loads the table (source-level only; `None`
    /// when unconfigured — dbt's `""` normalizes to `None`).
    pub loader: Option<String>,
    /// Effective `freshness.warn_after` — `None` when unconfigured (or
    /// cleared by a table's explicit `freshness: null`).
    pub freshness_warn_after: Option<FreshnessThreshold>,
    /// Effective `freshness.error_after` — `None` when unconfigured.
    pub freshness_error_after: Option<FreshnessThreshold>,
}

impl NodeDetail {
    /// Whether dbt has an effective freshness definition for this source:
    /// any configured `warn_after` or `error_after` counts (a threshold with
    /// a null count does not). This is the single predicate
    /// `sources_without_freshness` reads — loader-side precedence already
    /// resolved into the two fields.
    pub fn has_freshness(&self) -> bool {
        self.freshness_warn_after.is_some() || self.freshness_error_after.is_some()
    }
}

/// A directed dependency edge `parent -> child` inside a subgraph (both
/// endpoints are guaranteed members of the subgraph's node set).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edge {
    /// Upstream endpoint (the dependency).
    pub parent: String,
    /// Downstream endpoint (the dependent).
    pub child: String,
}

/// The lineage subgraph of a selected node: its ancestors (upstream closure),
/// the node itself, its descendants (downstream closure), and the dependency
/// edges among that node set.
///
/// This is the **input to layout**. It is a plain value type with no ratatui
/// or `Dag` coupling, so `layout()` can be unit-tested by constructing a
/// `Subgraph` literal directly (e.g. the asymmetric `a->b, a->c, b->c`
/// longest-path discriminator).
///
/// Determinism: `nodes` and `edges` are sorted (by `unique_id`, then
/// parent/child) so two `Dag::subgraph` calls on the same selection produce
/// bit-identical `Subgraph`s despite the `HashSet` source of the closures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subgraph {
    /// The selected node's `unique_id` (always a member of `nodes`).
    pub selected: String,
    /// All subgraph node infos (ancestors + selected + descendants), sorted by
    /// `unique_id` for deterministic ordering.
    pub nodes: Vec<NodeInfo>,
    /// Dependency edges `parent -> child` with both endpoints in `nodes`,
    /// sorted (parent, then child) for determinism.
    pub edges: Vec<Edge>,
}

impl Subgraph {
    /// Whether the subgraph contains a node with this `unique_id`.
    pub fn contains(&self, unique_id: &str) -> bool {
        self.nodes.iter().any(|n| n.unique_id == unique_id)
    }
}

/// The internal DAG model built from a parsed manifest.
///
/// - `nodes`: unified `unique_id -> NodeInfo` map (every resource type except
///   the excluded `test` / `operation` / `analysis` / `check` — models,
///   sources, seeds, snapshots, and exposures).
/// - `parents` / `children`: adjacency lists pruned to the kept node set, so
///   neither keys nor neighbours ever reference an excluded node.
/// - `details`: per-node detail (materialized/schema/columns/…) for the
///   structure modal, keyed by `unique_id`. Hung off the DAG (not `NodeInfo`)
///   to keep the hot subgraph clone/equality path cheap.
/// - `tests`: per-node tests, captured **pre-prune** from the test nodes (which
///   are excluded from the DAG topology) so they survive without ever bridging
///   lineage (`model -> test -> model` stays broken).
#[derive(Debug, Clone)]
pub struct Dag {
    nodes: HashMap<String, NodeInfo>,
    parents: HashMap<String, Vec<String>>,
    children: HashMap<String, Vec<String>>,
    details: HashMap<String, NodeDetail>,
    tests: HashMap<String, Vec<TestInfo>>,
    /// Per-node raw (uncompiled) SQL, keyed by `unique_id`. Hung off the DAG as
    /// a side map (like `details`/`tests`) so it never widens the hot subgraph
    /// clone/equality path nor `NodeDetail`'s frozen literals — and so the
    /// manifest↔source consistency comparison (which is field-by-field over
    /// details, never the whole `Dag`) is untouched. Sources/seeds have none.
    sql: HashMap<String, String>,
    /// Per-node compiled (Jinja-expanded) SQL + freshness, keyed by
    /// `unique_id` — the same side-map pattern as `sql`. Sources are the
    /// manifest's `compiled_code` (only present in a compile-produced
    /// manifest) or, falling that, `target/compiled/<pkg>/<original_file_path>`
    /// filled by [`Dag::ingest_compiled`]. Sources/seeds/exposures never have
    /// an entry; a node without any compiled artifact simply has none.
    compiled: HashMap<String, CompiledSql>,
    /// Per-exposure payload (kind / owner / url), keyed by `unique_id` — the
    /// same side-map pattern as `details`/`tests`/`sql`. Only exposure nodes
    /// have an entry.
    exposures: HashMap<String, ExposureInfo>,
    /// The manifest's macros, keyed by macro `unique_id`
    /// (`macro.<project>.<name>`). Empty for synthesized source-mode manifests
    /// (which never parse macros) — macro-diff detection keys off this.
    macros: BTreeMap<String, MacroInfo>,
    /// The macro `unique_id`s each kept node's compiled SQL calls (the node's
    /// `depends_on.macros`), a side map like `sql`/`details`.
    node_macros: HashMap<String, Vec<String>>,
    /// The ROOT project's name as dbt recorded it (`manifest.metadata.
    /// project_name`, present since dbt 1.6; `source.rs` synthesizes it from
    /// `dbt_project.yml`'s `name`). `None` for old/hand-built manifests —
    /// `project_name`/`primary_package` then fall back to inferring it from
    /// node `unique_id`s. Not a side map: one scalar for the whole graph,
    /// propagated by [`filtered`](Dag::filtered) so a `--project-only` view
    /// keeps naming itself after the root project.
    root_project: Option<String>,
    /// The adapter the manifest was compiled against (`metadata.adapter_type`
    /// — `duckdb`, `redshift`, …). `None` in source mode and for hand-built
    /// manifests; CLL uses it to pick the SQL dialect. One scalar, propagated
    /// by [`filtered`](Dag::filtered) like [`root_project`](Dag::project_name).
    adapter_type: Option<String>,
    /// The project's `{% docs __overview__ %}` body — the overview text dbt
    /// renders atop its own docs index. One scalar (like `root_project`):
    /// `Dag::build` picks it out of the manifest's `docs` map — the ROOT
    /// project's own entry (`doc.<proj>.__overview__`), never an installed
    /// package's (`doc.dbt.__overview__`, the boilerplate dbt ships in every
    /// manifest, can only win when the project is literally named `dbt`).
    /// [`filtered`](Dag::filtered) carries it over.
    overview: Option<String>,
    /// Per-kept-node adjacency the `PackageScope::Root` prune severed
    /// (installed-package neighbours only — [`PackageScope::apply_with_warnings`]
    /// records it; plain [`filtered`](Dag::filtered) carries but never
    /// recomputes it). Empty when no package prune ever ran — source mode
    /// and `All`-scope loads among them.
    pruned_adjacent: HashMap<String, PrunedAdjacent>,
}

/// A manifest macro's diff-relevant payload. The [`Dag`] stores these keyed by
/// macro `unique_id`; [`MacroInfo::depends_on`] is the macro→macro call graph
/// the diff walks for transitive shared-macro impact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacroInfo {
    /// The macro's short name (`macro.project.name` → `name`), for the
    /// `macro:<name>` modified reason.
    pub name: String,
    /// The Jinja body (`macro_sql`) — the surface a shared-macro change moves.
    pub sql: Option<String>,
    /// Macro `unique_id`s this macro itself calls.
    pub depends_on: Vec<String>,
}

/// `nodes` `resource_type`s deliberately dropped from the unified map and the
/// DAG — the SINGLE exclusion set: [`is_excluded`] is plain membership here,
/// and [`unhandled_resource_types`] treats every member as handled so an
/// excluded kind never warns.
///
/// - `test`: folded into the `tests` side map pre-prune.
/// - `operation` / `analysis`: non-buildable; `analysis` is also dropped for
///   the manifest↔source consistency contract (`source.rs` never parses
///   `analyses/`).
/// - `check` (dbt Fusion's validation resource, `check.<pkg>.<name>`): the
///   same fate as `test` — a check declares a validation over models, not a
///   buildable lineage step, and its payload is dropped entirely (NOT folded
///   into `tests`, so `tests:N` counts and coverage are unmoved).
const EXCLUDED_NODE_RESOURCE_TYPES: &[&str] = &["test", "operation", "analysis", "check"];

/// `nodes` `resource_type`s kept and drawn deliberately — the buildable
/// lineage kinds.
const KEPT_NODE_RESOURCE_TYPES: &[&str] = &["model", "seed", "snapshot"];

/// The remaining `resource_type`s the dbt manifest schema defines (the v12
/// enum's `nodes` members plus the types the shared enum lists for the
/// surface maps) — real dbt types dbtl does not handle inside `nodes`.
/// They are KEPT as generic graph nodes — `sql_operation`/`rpc`/`function`
/// CAN carry `depends_on` adjacency (a run-operation's `ref()` edges are
/// real topology), so excluding them like [`EXCLUDED_NODE_RESOURCE_TYPES`]
/// would hide lineage — but [`unhandled_resource_types`] reports them as
/// "unsupported" so the box is never drawn silently.
const UNSUPPORTED_NODE_RESOURCE_TYPES: &[&str] = &[
    "doc",
    "exposure",
    "fixture",
    "function",
    "group",
    "macro",
    "metric",
    "rpc",
    "saved_query",
    "semantic_model",
    "source",
    "sql_operation",
    "unit_test",
];

/// Whether a `resource_type` is dropped from the graph. Membership in
/// [`EXCLUDED_NODE_RESOURCE_TYPES`] — keep the rationale there; the two stay
/// in sync by construction (one const, one predicate).
fn is_excluded(resource_type: &str) -> bool {
    EXCLUDED_NODE_RESOURCE_TYPES.contains(&resource_type)
}

/// Whether a `manifest.nodes` `resource_type` dbtl does not handle is a real
/// dbt type or something outside the schema entirely — the split behind the
/// two load warnings ("known but unsupported" vs "truly unknown").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceTypeSupport {
    /// A real dbt type (`UNSUPPORTED_NODE_RESOURCE_TYPES`) that lands in
    /// `nodes` — warned as `unsupported resource_type`.
    Known,
    /// A `resource_type` outside the dbt schema (a plugin type, a newer
    /// schema than this build) — warned as `unknown resource_type`.
    Unknown,
}

/// `None` for the handled types (kept buildable kinds and the deliberately
/// excluded kinds); otherwise which warning class a `nodes` type falls in.
fn unhandled_support(resource_type: &str) -> Option<ResourceTypeSupport> {
    if KEPT_NODE_RESOURCE_TYPES.contains(&resource_type) || is_excluded(resource_type) {
        None
    } else if UNSUPPORTED_NODE_RESOURCE_TYPES.contains(&resource_type) {
        Some(ResourceTypeSupport::Known)
    } else {
        Some(ResourceTypeSupport::Unknown)
    }
}

/// `(resource_type, count, support)` for every `manifest.nodes` type dbtl
/// does not handle, sorted by type for determinism: [`ResourceTypeSupport::Known`]
/// is a real dbt type dbtl does not classify (`unsupported` warning), and
/// [`ResourceTypeSupport::Unknown`] is outside the schema entirely (`unknown`
/// warning). Both kinds are KEPT as generic graph nodes. The surface maps
/// (`sources`/`exposures`/`semantic_models`/…) carry a fixed type by
/// construction, so only `nodes` is scanned.
pub fn unhandled_resource_types(
    manifest: &RawManifest,
) -> Vec<(String, usize, ResourceTypeSupport)> {
    // A kind maps to exactly one support class, so the support rides the
    // value while the kind stays the unique sort key.
    let mut counts: BTreeMap<&str, (usize, ResourceTypeSupport)> = BTreeMap::new();
    for node in manifest.nodes.values() {
        if let Some(support) = unhandled_support(&node.resource_type) {
            counts
                .entry(node.resource_type.as_str())
                .or_insert((0, support))
                .0 += 1;
        }
    }
    counts
        .into_iter()
        .map(|(kind, (n, support))| (kind.to_string(), n, support))
        .collect()
}

/// Convenience: load a manifest from disk and build its DAG in one call.
/// Discards load-time warnings — see [`load_dag_with_warnings`].
///
/// Also auto-detects the compiled-SQL side map: `target/compiled/` next to
/// the manifest (same sibling-artifact pattern as `run_results.json` /
/// `catalog.json`). Missing dirs/files are simply "no compiled SQL", never
/// an error.
pub fn load_dag<P: AsRef<Path>>(path: P) -> Result<Dag> {
    load_dag_with_warnings(path).map(|(dag, _)| dag)
}

/// Like [`load_dag`], but also returns load-time warnings: one line per
/// unhandled `resource_type` found in `manifest.nodes` (see
/// [`unhandled_resource_types`] — `unsupported` for a real dbt type dbtl
/// does not classify, `unknown` for a type outside the schema), plus one
/// line for a non-empty `functions` map (UDFs are never graph nodes). All
/// of these are KEPT as generic graph nodes and the warnings say so — dbtl
/// is a viewer, so silently dropping a resource it does not understand
/// would hide real topology, while silently keeping it would draw an
/// unexplained plain box.
pub fn load_dag_with_warnings<P: AsRef<Path>>(path: P) -> Result<(Dag, Vec<String>)> {
    let path = path.as_ref();
    let manifest = load_manifest(path)?;
    let mut warnings: Vec<String> = unhandled_resource_types(&manifest)
        .into_iter()
        .map(|(kind, n, support)| {
            let class = match support {
                ResourceTypeSupport::Known => "unsupported",
                ResourceTypeSupport::Unknown => "unknown",
            };
            format!(
                "{class} resource_type '{kind}' ({n} node{}): kept as generic graph nodes",
                if n == 1 { "" } else { "s" }
            )
        })
        .collect();
    if !manifest.functions.is_empty() {
        warnings.push(format!(
            "'functions' map ({} function{}): not displayed",
            manifest.functions.len(),
            if manifest.functions.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    let mut dag = Dag::build(&manifest);
    if let Some(compiled_dir) = crate::compiled::default_compiled_dir(Some(path), None) {
        // The staleness root is the project root = the manifest dir's parent
        // — but only when that dir is literally named `target/` (the standard
        // layout `<root>/target/manifest.json`). A manifest elsewhere (a
        // copied artifact, `--manifest /tmp/m.json`) still reads its sibling
        // compiled/ tree, yet `..` would resolve to / or an arbitrary parent,
        // making mtime-vs-source comparisons against the wrong tree —
        // undeterminable means not stale.
        let root = path
            .parent()
            .filter(|d| d.file_name().is_some_and(|n| n == "target"))
            .and_then(|d| d.parent());
        dag.ingest_compiled(&compiled_dir, root);
    }
    Ok((dag, warnings))
}

mod build;
mod convert;
mod graph;
mod query;
#[cfg(test)]
mod tests;
mod traverse;

pub use convert::in_package;
use convert::*;
use traverse::*;
