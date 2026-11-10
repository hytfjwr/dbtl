//! The pruned dependency DAG and its value types: merge a parsed manifest's
//! `nodes` (excluding `test` / `operation` / `analysis` / `check`), `sources`,
//! and `exposures` into a single `unique_id -> NodeInfo` map, build pruned
//! adjacency over the kept set, and compute upstream / downstream transitive
//! closures (cycle-safe).
//! Side maps (`details` / `tests` / `sql` / `compiled` / `exposures`) are
//! captured pre-prune as data only — they never touch the topology.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::compiled::CompiledSql;
use crate::manifest::{load_manifest, RawExposure, RawMacro, RawManifest, RawNode, RawSource};

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

/// Whether a node is a declared downstream consumer (an exposure). Pure over
/// [`NodeInfo::resource_type`] — the single source of truth for the exposure
/// predicate (c.f. [`coverage_gap`]), so the impact split and the report can
/// never disagree on what counts as one.
pub fn is_exposure(n: &NodeInfo) -> bool {
    n.resource_type == "exposure"
}

/// One column of a model/source, as recorded in the manifest. Kept in dbt
/// *definition* order (the manifest preserves it once `serde_json`'s
/// `preserve_order` feature is on).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: Option<String>,
    pub description: Option<String>,
}

/// A test attached to a node, captured *pre-prune* (tests are excluded from the
/// DAG, so this is the only place they survive). `kind` is the generic test
/// name (`unique`, `not_null`, `relationships`, …) or `"singular"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestInfo {
    pub name: String,
    pub kind: String,
    pub column_name: Option<String>,
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

/// The detail payload for a node, used by the structure modal and the status
/// line. Hung off the [`Dag`] (NOT [`NodeInfo`]) so the hot subgraph clone /
/// equality path stays cheap and the frozen `NodeInfo` literals are untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeDetail {
    pub materialized: Option<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub columns: Vec<ColumnInfo>,
    pub original_file_path: Option<String>,
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
    /// This node's version name, when the model is versioned.
    pub version: Option<String>,
    /// The newest version's name, when the model is versioned.
    pub latest_version: Option<String>,
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

/// Resource types that are excluded from the unified map and the DAG.
///
/// `analysis` is excluded alongside `test`/`operation` because `source.rs`
/// never parses `analyses/` at all — showing it only in manifest mode would
/// break the manifest↔source consistency contract (the same graph either way).
/// `check` (dbt Fusion's validation resource, `check.<pkg>.<name>`) gets the
/// same fate as `test`: it declares a check over models, not a buildable
/// lineage step, and its payload is dropped entirely — it is NOT folded into
/// the `tests` side map, so it never moves the `tests:N` counts or coverage.
fn is_excluded(resource_type: &str) -> bool {
    matches!(resource_type, "test" | "operation" | "analysis" | "check")
}

/// The `resource_type`s [`Dag::build`] handles deliberately inside
/// `manifest.nodes`: the kept buildable resources plus the excluded non-graph
/// kinds. Any OTHER type (a newer dbt/Fusion resource such as `rpc` or
/// `notebook`, or a plugin type) still joins the graph as a generic node —
/// [`unknown_resource_types`] reports it at load time so a type dbtl does not
/// understand is never drawn silently.
const KNOWN_NODE_RESOURCE_TYPES: &[&str] = &[
    "model",
    "seed",
    "snapshot",
    "test",
    "operation",
    "analysis",
    "check",
];

/// `(resource_type, count)` for every type in `manifest.nodes` outside
/// [`KNOWN_NODE_RESOURCE_TYPES`], sorted by type for determinism. Deliberately
/// excluded types (`test`/`check`/…) are KNOWN, so they never warn. The
/// surfaces maps (`sources`/`exposures`/`semantic_models`/…) carry a fixed
/// type by construction, so only `nodes` is scanned.
pub fn unknown_resource_types(manifest: &RawManifest) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in manifest.nodes.values() {
        if !KNOWN_NODE_RESOURCE_TYPES.contains(&node.resource_type.as_str()) {
            *counts.entry(node.resource_type.as_str()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|(kind, n)| (kind.to_string(), n))
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
/// unknown `resource_type` found in `manifest.nodes` (see
/// [`unknown_resource_types`]). Unknown types are KEPT as generic graph
/// nodes and the warning says so — dbtl is a viewer, so silently dropping a
/// resource it does not understand would hide real topology, while silently
/// keeping it would draw an unexplained plain box.
pub fn load_dag_with_warnings<P: AsRef<Path>>(path: P) -> Result<(Dag, Vec<String>)> {
    let path = path.as_ref();
    let manifest = load_manifest(path)?;
    let warnings: Vec<String> = unknown_resource_types(&manifest)
        .into_iter()
        .map(|(kind, n)| {
            format!(
                "unknown resource_type '{kind}' ({n} node{}): kept as generic graph nodes",
                if n == 1 { "" } else { "s" }
            )
        })
        .collect();
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

impl Dag {
    /// Build the DAG from a parsed manifest.
    ///
    /// Excludes `test` / `operation` / `analysis` / `check` nodes, merges
    /// `nodes` + `sources` into a single map, and prunes `parent_map` / `child_map` down
    /// to the kept set
    /// (both keys and neighbour lists) so traversal never touches an excluded
    /// node as an endpoint or as an intermediate hop.
    ///
    /// Two side maps are also built: `details` (per kept node) and `tests`. The
    /// `tests` map is populated **pre-prune** — while walking the raw nodes,
    /// before any topology is built — purely as a recording keyed by the test's
    /// target node(s); it never touches `parent_map` / `child_map`, so a test is
    /// never a transit hop.
    pub fn build(manifest: &RawManifest) -> Self {
        let mut nodes: HashMap<String, NodeInfo> = HashMap::new();
        let mut details: HashMap<String, NodeDetail> = HashMap::new();
        let mut tests: HashMap<String, Vec<TestInfo>> = HashMap::new();
        let mut sql: HashMap<String, String> = HashMap::new();
        let mut compiled: HashMap<String, CompiledSql> = HashMap::new();
        let mut exposures: HashMap<String, ExposureInfo> = HashMap::new();
        let mut node_macros: HashMap<String, Vec<String>> = HashMap::new();

        for (unique_id, node) in &manifest.nodes {
            if node.resource_type == "test" {
                register_test(&mut tests, node); // capture pre-prune, then skip
                continue;
            }
            if is_excluded(&node.resource_type) {
                continue;
            }
            if !node.depends_on.macros.is_empty() {
                node_macros.insert(unique_id.clone(), node.depends_on.macros.clone());
            }
            nodes.insert(
                unique_id.clone(),
                NodeInfo {
                    unique_id: unique_id.clone(),
                    name: node.name.clone(),
                    resource_type: node.resource_type.clone(),
                    path: node.path.clone(),
                    materialized: node.config.materialized.clone(),
                    ..Default::default() // direct_up/down filled after pruning below
                },
            );
            details.insert(unique_id.clone(), detail_from_node(node));
            if let Some(code) = &node.raw_code {
                sql.insert(unique_id.clone(), code.clone());
            }
            // Manifest-carried compiled SQL: only a compile/build-produced
            // manifest fills `compiled_code` (dbt parse leaves it null).
            // Non-empty only — an empty field falls through to the
            // `target/compiled/` files like an absent one. Never stale: it
            // ships in the same artifact as `raw_code`.
            if let Some(code) = node.compiled_code.as_ref().filter(|c| !c.trim().is_empty()) {
                compiled.insert(
                    unique_id.clone(),
                    CompiledSql {
                        code: code.clone(),
                        stale: false,
                    },
                );
            }
        }

        for (unique_id, source) in &manifest.sources {
            // Sources are never test/operation, but guard for completeness.
            if is_excluded(&source.resource_type) {
                continue;
            }
            nodes.insert(
                unique_id.clone(),
                NodeInfo {
                    unique_id: unique_id.clone(),
                    name: source.name.clone(),
                    resource_type: source.resource_type.clone(),
                    path: source.path.clone(),
                    materialized: None,
                    ..Default::default() // direct_up/down filled after pruning below
                },
            );
            details.insert(unique_id.clone(), detail_from_source(source));
        }

        for (unique_id, exposure) in &manifest.exposures {
            nodes.insert(
                unique_id.clone(),
                NodeInfo {
                    unique_id: unique_id.clone(),
                    name: exposure.name.clone(),
                    resource_type: "exposure".to_string(),
                    path: exposure.path.clone(),
                    materialized: None,
                    ..Default::default() // direct_up/down filled after pruning below
                },
            );
            details.insert(unique_id.clone(), detail_from_exposure(exposure));
            exposures.insert(
                unique_id.clone(),
                ExposureInfo {
                    exposure_type: exposure.exposure_type.clone(),
                    owner_name: exposure.owner.name.clone(),
                    owner_email: exposure.owner.email.clone(),
                    url: exposure.url.clone(),
                },
            );
        }

        // Semantic-layer resources (semantic models / metrics / saved queries)
        // — declared downstream consumers, exactly like exposures: terminator
        // nodes fed by their own `depends_on` (metrics chain off semantic
        // models, which is fine — all three maps become nodes).
        for (map, resource_type) in [
            (&manifest.semantic_models, "semantic_model"),
            (&manifest.metrics, "metric"),
            (&manifest.saved_queries, "saved_query"),
        ] {
            for (unique_id, res) in map {
                nodes.insert(
                    unique_id.clone(),
                    NodeInfo {
                        unique_id: unique_id.clone(),
                        name: res.name.clone(),
                        resource_type: resource_type.to_string(),
                        path: res.path.clone(),
                        materialized: None,
                        ..Default::default()
                    },
                );
                details.insert(
                    unique_id.clone(),
                    NodeDetail {
                        description: clean_desc(&res.description),
                        original_file_path: res.original_file_path.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        // dbt ≥1.8 unit tests: excluded from the DAG like schema tests,
        // recorded per-target model. A unit_test uid is
        // `unit_tests.<pkg>.<model>.<name>` — the target is `model.<pkg>.<model>`;
        // `model` (the bare name) is the fallback when the uid shape differs.
        for (uid, ut) in &manifest.unit_tests {
            let mut segs = uid.split('.');
            let target = match (segs.nth(1), segs.next()) {
                (Some(pkg), Some(model)) if !pkg.is_empty() => {
                    let guess = format!("model.{pkg}.{model}");
                    if nodes.contains_key(&guess) {
                        Some(guess)
                    } else {
                        ut.model.as_ref().and_then(|m| model_uid_by_name(&nodes, m))
                    }
                }
                _ => ut.model.as_ref().and_then(|m| model_uid_by_name(&nodes, m)),
            };
            if let Some(target) = target {
                tests.entry(target).or_default().push(TestInfo {
                    name: ut.name.clone(),
                    kind: "unit".to_string(),
                    column_name: None,
                });
            }
        }

        // Deterministic test ordering per node (HashMap iteration is unordered).
        for v in tests.values_mut() {
            v.sort_by(|a, b| {
                a.kind
                    .cmp(&b.kind)
                    .then_with(|| a.column_name.cmp(&b.column_name))
                    .then_with(|| a.name.cmp(&b.name))
            });
        }

        let kept: HashSet<&String> = nodes.keys().collect();
        let mut parents = prune_adjacency(&manifest.parent_map, &kept);
        let mut children = prune_adjacency(&manifest.child_map, &kept);

        // Exposure edges come from BOTH the manifest's parent/child maps (a real
        // dbt manifest records them there, pruned above) AND each exposure's own
        // `depends_on` (the only adjacency a synthesized source-mode manifest
        // fills), deduplicated — so both modes converge on the same graph.
        // Iterated in sorted uid order so the appended-edge order (and thus the
        // adjacency lists) is deterministic. Exposures only ever gain PARENTS:
        // they are terminator leaves by construction.
        let mut exposure_ids: Vec<&String> = manifest.exposures.keys().collect();
        exposure_ids.sort();
        for uid in exposure_ids {
            for dep in &manifest.exposures[uid].depends_on.nodes {
                if !kept.contains(dep) {
                    continue; // a dangling dep is dropped, like an unresolved ref
                }
                let p = parents.entry(uid.clone()).or_default();
                if !p.contains(dep) {
                    p.push(dep.clone());
                }
                let c = children.entry(dep.clone()).or_default();
                if !c.contains(uid) {
                    c.push(uid.clone());
                }
            }
        }
        // Semantic-layer resources: same terminator-edge rule — real manifests
        // already carry them in parent/child_map, but `depends_on` is the
        // guaranteed adjacency (metrics may even reference semantic models).
        let mut semantic_edges: Vec<(&String, &Vec<String>)> = Vec::new();
        for map in [
            &manifest.semantic_models,
            &manifest.metrics,
            &manifest.saved_queries,
        ] {
            for (uid, res) in map {
                semantic_edges.push((uid, &res.depends_on.nodes));
            }
        }
        semantic_edges.sort_by_key(|(uid, _)| *uid);
        for (uid, deps) in semantic_edges {
            for dep in deps {
                if !kept.contains(dep) {
                    continue; // a dangling dep is dropped, like an unresolved ref
                }
                let p = parents.entry(uid.clone()).or_default();
                if !p.contains(dep) {
                    p.push(dep.clone());
                }
                let c = children.entry(dep.clone()).or_default();
                if !c.contains(uid) {
                    c.push(uid.clone());
                }
            }
        }

        // Stamp each node's direct (1-hop) parent/child counts from the pruned
        // adjacency (list-pane dependency badges + orphan detection) and its
        // tests count (the lineage box's `tests:N` label).
        for (uid, info) in nodes.iter_mut() {
            info.direct_up = parents.get(uid).map_or(0, |v| v.len());
            info.direct_down = children.get(uid).map_or(0, |v| v.len());
            info.test_count = tests.get(uid).map_or(0, |v| v.len());
        }

        // The macro side map: every macro the manifest declares, keyed by its
        // `unique_id`. Sorted-map storage so macro-driven output is ordered.
        let macros: BTreeMap<String, MacroInfo> = manifest
            .macros
            .iter()
            .map(|(uid, m)| (uid.clone(), macro_info(m)))
            .collect();

        Dag {
            nodes,
            parents,
            children,
            details,
            tests,
            sql,
            compiled,
            exposures,
            macros,
            node_macros,
            // A blank metadata name is as good as absent — the same guard the
            // `unique_id`-segment fallbacks apply (`!seg.is_empty()`), plus
            // whitespace-only.
            root_project: manifest
                .metadata
                .project_name
                .clone()
                .filter(|s| !s.trim().is_empty()),
        }
    }

    /// Fill the `compiled` side map from `target/compiled/` — the
    /// Jinja-expanded SQL `dbt compile`/`build`/`run`/`test` writes per node
    /// as `<compiled_dir>/<package>/<original_file_path>`. The disk loaders
    /// ([`load_dag`], `source::load_dag_from_source_with_warnings`) call this
    /// automatically; `Dag::build` itself stays pure (manifest-only) so a
    /// `RawManifest` literal is all a test needs.
    ///
    /// Per-node resolution order: a non-empty manifest `compiled_code`
    /// already in the map wins (the same artifact as `raw_code`); otherwise
    /// the on-disk file. Missing/unreadable files and whitespace-only
    /// contents are skipped — absence is never an error. A file read is
    /// marked stale when its source is newer than the expansion — only when
    /// `project_root` is `Some` (a `None` root means the staleness root is
    /// undeterminable — e.g. a manifest not living in a `target/` dir).
    /// Deterministic: each entry depends only on its own node's coordinates,
    /// never on iteration order.
    pub fn ingest_compiled(&mut self, compiled_dir: &Path, project_root: Option<&Path>) {
        if !compiled_dir.is_dir() {
            return;
        }
        // Computed once: the package that owns this graph decides whether a
        // node's ofp is project-root-relative (local) or package-root-relative
        // (installed → `dbt_packages/`/`dbt_internal_packages/` only — never
        // the project root, where an unrelated file sharing the path would
        // flag a false stale). `package_name.is_none()` nodes are local too:
        // comparing to the primary segment yields the same answer.
        let primary = self.primary_package();
        for (uid, detail) in &self.details {
            if self.compiled.contains_key(uid) {
                continue; // manifest compiled_code already won
            }
            let Some(ofp) = detail.original_file_path.as_deref() else {
                continue;
            };
            // Compiled artifacts are .sql files — plus the `.py` bodies dbt
            // keeps for Python models (Snowpark/Databricks), which run
            // unexpanded. A source's `.yml` or a seed's `.csv` `original_file_path`
            // can never resolve.
            if !(ofp.ends_with(".sql") || ofp.ends_with(".py")) {
                continue;
            }
            // The package segment: `package_name` in a real manifest (an
            // empty string falls through to the uid segment — dbt never
            // writes one, a hand-rolled one might); the uid's second segment
            // covers synthesized source-mode nodes (which leave
            // `package_name` None — every node is local).
            let Some(pkg) = detail
                .package_name
                .as_deref()
                .filter(|p| !p.is_empty())
                .or_else(|| uid.split('.').nth(1))
            else {
                continue;
            };
            let Some(path) = crate::compiled::compiled_file_path(compiled_dir, pkg, ofp) else {
                continue;
            };
            let Ok(code) = std::fs::read_to_string(&path) else {
                continue;
            };
            if code.trim().is_empty() {
                continue;
            }
            let local = primary.as_deref().is_none_or(|p| p == pkg);
            let stale = project_root
                .is_some_and(|root| crate::compiled::is_stale(&path, root, local, pkg, ofp));
            self.compiled
                .insert(uid.clone(), CompiledSql { code, stale });
        }
    }

    /// The manifest's macros (`macro.<project>.<name>` → [`MacroInfo`]). Empty
    /// for a synthesized source-mode manifest — macro-diff detection is
    /// manifest-vs-manifest only, by design.
    pub fn macros(&self) -> &BTreeMap<String, MacroInfo> {
        &self.macros
    }

    /// The macro `unique_id`s a kept node's compiled SQL calls
    /// (`depends_on.macros`). Empty for nodes that call no macro — and for
    /// every node in source mode.
    pub fn node_macros(&self, unique_id: &str) -> &[String] {
        self.node_macros.get(unique_id).map_or(&[], Vec::as_slice)
    }

    /// The unified `unique_id -> NodeInfo` map (read-only access).
    pub fn nodes(&self) -> &HashMap<String, NodeInfo> {
        &self.nodes
    }

    /// Number of entries in the unified map.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the unified map is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether the given `unique_id` is present in the unified map.
    pub fn contains(&self, unique_id: &str) -> bool {
        self.nodes.contains_key(unique_id)
    }

    /// Look up a node by `unique_id`.
    pub fn get(&self, unique_id: &str) -> Option<&NodeInfo> {
        self.nodes.get(unique_id)
    }

    /// Count of kept nodes with the given `resource_type`.
    pub fn count_by_resource_type(&self, resource_type: &str) -> usize {
        self.nodes
            .values()
            .filter(|n| n.resource_type == resource_type)
            .count()
    }

    /// The dbt project name — the ROOT project's name when the manifest
    /// records it (`metadata.project_name`, dbt ≥1.6; source mode synthesizes
    /// it from `dbt_project.yml`). That is the authoritative answer: an
    /// installed package whose name sorts first in `unique_id`s can no longer
    /// masquerade as the project.
    ///
    /// Fallback (old/hand-built manifests with no metadata): the
    /// lexicographically-smallest `<project>` segment of the node
    /// `unique_id`s (`<resource_type>.<project>.<name>`) — deterministic and
    /// stable for the usual single-project manifest. `None` when no node
    /// carries a project segment.
    ///
    /// Shared by the docs index heading and the title-bar stats so the two
    /// never disagree.
    pub fn project_name(&self) -> Option<&str> {
        self.root_project.as_deref().or_else(|| {
            self.nodes
                .keys()
                .filter_map(|uid| uid.split('.').nth(1))
                .filter(|seg| !seg.is_empty())
                .min()
        })
    }

    /// The detail payload (materialized / schema / columns / …) for a node, or
    /// `None` if unknown. Used by the structure modal and the status line.
    pub fn detail(&self, unique_id: &str) -> Option<&NodeDetail> {
        self.details.get(unique_id)
    }

    /// The kept 1-hop parent uids of `unique_id` (empty slice when none or
    /// unknown). Order is the pruned adjacency's — deterministic per manifest.
    /// The column-lineage engine resolves each node's edges against exactly
    /// this set.
    pub fn direct_parents(&self, unique_id: &str) -> &[String] {
        self.parents.get(unique_id).map_or(&[], Vec::as_slice)
    }

    /// The kept 1-hop child uids of `unique_id` (empty slice when none or
    /// unknown) — the counterpart of [`direct_parents`](Dag::direct_parents),
    /// for callers scanning a node's consumers (column-lineage downstream
    /// queries) without a full subgraph build.
    pub fn direct_children(&self, unique_id: &str) -> &[String] {
        self.children.get(unique_id).map_or(&[], Vec::as_slice)
    }

    /// Test-only seam: inject a compiled-SQL entry directly. A `stale` flag
    /// cannot be expressed in a manifest literal, and forging
    /// `target/compiled/` mtimes inside a unit test needs `filetime`-grade APIs
    /// the crate does not carry.
    #[cfg(test)]
    pub fn set_compiled_for_test(&mut self, unique_id: &str, code: &str, stale: bool) {
        self.compiled.insert(
            unique_id.to_string(),
            CompiledSql {
                code: code.to_string(),
                stale,
            },
        );
    }

    /// The name of the package that OWNS this graph — the ROOT project's name
    /// when the manifest records it (see [`project_name`](Dag::project_name)),
    /// else the most frequent `unique_id` project segment
    /// (`model.<pkg>.<name>`), robust to installed packages that happen to
    /// sort first; ties break to the smaller name. Drives `--project-only`
    /// (which therefore scopes to the root project, not the biggest package).
    pub fn primary_package(&self) -> Option<String> {
        if let Some(root) = &self.root_project {
            return Some(root.clone());
        }
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for uid in self.nodes.keys() {
            if let Some(seg) = uid.split('.').nth(1).filter(|s| !s.is_empty()) {
                *counts.entry(seg).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by(|(a, ca), (b, cb)| ca.cmp(cb).then_with(|| b.cmp(a)))
            .map(|(name, _)| name.to_string())
    }

    /// The sub-Dag containing only nodes `keep` accepts — every side map
    /// (`details`/`tests`/`sql`/`exposures`/`node_macros`) and every edge
    /// endpoint pruned to the same uid set, so no dangling reference survives.
    /// Deterministic (same maps, subset). Powers `--project-only`.
    pub fn filtered(&self, keep: impl Fn(&NodeInfo) -> bool) -> Dag {
        let nodes: HashMap<String, NodeInfo> = self
            .nodes
            .iter()
            .filter(|(_, n)| keep(n))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let prune_map = |m: &HashMap<String, Vec<String>>,
                         keep: &HashMap<String, NodeInfo>|
         -> HashMap<String, Vec<String>> {
            m.iter()
                .filter(|(k, _)| keep.contains_key(*k))
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter()
                            .filter(|u| keep.contains_key(u.as_str()))
                            .cloned()
                            .collect(),
                    )
                })
                .collect()
        };
        let parents = prune_map(&self.parents, &nodes);
        let children = prune_map(&self.children, &nodes);
        // direct_up/direct_down on NodeInfo must reflect the SURVIVING edges,
        // not the pre-filter topology.
        let mut nodes = nodes;
        for (uid, info) in nodes.iter_mut() {
            info.direct_up = parents.get(uid).map_or(0, Vec::len);
            info.direct_down = children.get(uid).map_or(0, Vec::len);
        }
        Dag {
            parents,
            children,
            details: self
                .details
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            tests: self
                .tests
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            sql: self
                .sql
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            compiled: self
                .compiled
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            exposures: self
                .exposures
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // The macro registry is small and shared: keep it whole so
            // surviving nodes' macro deps still resolve.
            macros: self.macros.clone(),
            node_macros: self
                .node_macros
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // The root identity is a property of the source manifest, not of
            // the kept uid set — a `--project-only` view is still that project.
            root_project: self.root_project.clone(),
            nodes,
        }
    }

    /// The tests attached to a node (empty slice when none). Captured pre-prune,
    /// sorted by `(kind, column, name)` for determinism.
    pub fn tests(&self, unique_id: &str) -> &[TestInfo] {
        self.tests.get(unique_id).map_or(&[], |v| v.as_slice())
    }

    /// The raw (uncompiled) SQL for a node, or `None` (sources/seeds, or models
    /// loaded from a manifest that omitted `raw_code`). Stored as a side map so
    /// it never widens the hot subgraph clone/equality path nor `NodeDetail`'s
    /// frozen literals.
    pub fn raw_code(&self, unique_id: &str) -> Option<&str> {
        self.sql.get(unique_id).map(String::as_str)
    }

    /// The compiled-SQL side-map entry for a node (expanded text + `stale`
    /// freshness flag), or `None` when neither the manifest's `compiled_code`
    /// nor a `target/compiled/` file covered it. Counterpart of
    /// [`detail`](Dag::detail) for the [`compiled_code`](Dag::compiled_code)
    /// accessor.
    pub fn compiled(&self, unique_id: &str) -> Option<&CompiledSql> {
        self.compiled.get(unique_id)
    }

    /// The compiled (Jinja-expanded) SQL text for a node, or `None` — the
    /// counterpart of [`raw_code`](Dag::raw_code). Resolution order:
    /// non-empty manifest `compiled_code`, then `target/compiled/` (see
    /// [`Dag::ingest_compiled`]).
    pub fn compiled_code(&self, unique_id: &str) -> Option<&str> {
        self.compiled.get(unique_id).map(|c| c.code.as_str())
    }

    /// The exposure payload (kind / owner / url) for an exposure node, or
    /// `None` for every other node. Side map, like [`detail`](Dag::detail).
    pub fn exposure(&self, unique_id: &str) -> Option<&ExposureInfo> {
        self.exposures.get(unique_id)
    }

    /// Transitive closure of ancestors (upstream) of `start`, excluding `start`.
    ///
    /// Multi-hop, cycle-safe, deduplicated. Returns an empty set when `start`
    /// is unknown or has no kept ancestors.
    pub fn upstream(&self, start: &str) -> HashSet<String> {
        closure(&self.parents, start)
    }

    /// Transitive closure of descendants (downstream) of `start`, excluding `start`.
    ///
    /// Multi-hop, cycle-safe, deduplicated. Returns an empty set when `start`
    /// is unknown or has no kept descendants.
    pub fn downstream(&self, start: &str) -> HashSet<String> {
        closure(&self.children, start)
    }

    /// Every direct (1-hop) `(parent, child)` edge in the pruned DAG, sorted for
    /// determinism. Exposes the pruned adjacency as concrete edges without making
    /// `children` public, so callers (e.g. `app::layer_violation_edges`) can scan
    /// the whole graph's edges in a stable order. Keyed off `children` so each
    /// edge is listed exactly once.
    pub fn edges(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .children
            .iter()
            .flat_map(|(parent, kids)| kids.iter().map(move |c| (parent.clone(), c.clone())))
            .collect();
        out.sort();
        out
    }

    /// Build the lineage [`Subgraph`] for `selected`: its upstream closure, the
    /// node itself, its downstream closure, and every dependency edge whose
    /// endpoints are both in that node set.
    ///
    /// Exposes the pruned adjacency as concrete edges without making
    /// `parents`/`children` public. Both `nodes` and `edges` are sorted for
    /// determinism (the closures come from `HashSet`s, which are unordered), so
    /// the result is reproducible.
    ///
    /// Returns an empty subgraph (no nodes) when `selected` is unknown.
    pub fn subgraph(&self, selected: &str) -> Subgraph {
        self.subgraph_view(selected, true, true, None)
    }

    /// Like [`subgraph`](Dag::subgraph) but with a directional / depth-limited
    /// view: include the `upstream` and/or `downstream` sides, each limited to
    /// `depth` hops (`None` = unlimited). The selected node is always present.
    /// Edges are kept only between nodes in the resulting set, so the layout
    /// invariants (right-going, non-overlap) hold for any view.
    pub fn subgraph_view(
        &self,
        selected: &str,
        upstream: bool,
        downstream: bool,
        depth: Option<usize>,
    ) -> Subgraph {
        if !self.nodes.contains_key(selected) {
            return Subgraph {
                selected: selected.to_string(),
                nodes: Vec::new(),
                edges: Vec::new(),
            };
        }

        // Node set = (upstream side?) ∪ {selected} ∪ (downstream side?), each
        // closure limited to `depth` hops.
        let mut id_set: HashSet<String> = HashSet::new();
        if upstream {
            id_set.extend(closure_depth(&self.parents, selected, depth));
        }
        if downstream {
            id_set.extend(closure_depth(&self.children, selected, depth));
        }
        id_set.insert(selected.to_string());

        // Materialize NodeInfos, sorted by unique_id for a deterministic order.
        let mut nodes: Vec<NodeInfo> = id_set
            .iter()
            .filter_map(|id| self.nodes.get(id).cloned())
            .collect();
        nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));

        // Edges: for each kept child, its parents that are also in the set.
        // Using `parents` keeps each edge listed once (keyed by child).
        let mut edges: Vec<Edge> = Vec::new();
        for node in &nodes {
            if let Some(parents) = self.parents.get(&node.unique_id) {
                for parent in parents {
                    if id_set.contains(parent) {
                        edges.push(Edge {
                            parent: parent.clone(),
                            child: node.unique_id.clone(),
                        });
                    }
                }
            }
        }
        edges.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.child.cmp(&b.child)));
        edges.dedup();

        Subgraph {
            selected: selected.to_string(),
            nodes,
            edges,
        }
    }

    /// The whole-project [`Subgraph`]: every kept node (sorted by `unique_id`)
    /// and every pruned 1-hop edge (sorted, via [`edges`](Dag::edges)).
    /// `selected` is left EMPTY — the interactive overview sets it to the
    /// current selection afterwards; the docs generator keeps it empty so no
    /// node is emphasised.
    pub fn full_subgraph(&self) -> Subgraph {
        let mut nodes: Vec<NodeInfo> = self.nodes.values().cloned().collect();
        nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));
        let edges = self
            .edges()
            .into_iter()
            .map(|(parent, child)| Edge { parent, child })
            .collect();
        Subgraph {
            selected: String::new(),
            nodes,
            edges,
        }
    }
}

/// Trim a description (strip surrounding whitespace / trailing newlines), turning
/// an empty one into `None`.
fn clean_desc(desc: &Option<String>) -> Option<String> {
    desc.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Build [`ColumnInfo`]s from a manifest `columns` object, preserving definition
/// order (the `Map` is order-preserving via `serde_json/preserve_order`). The
/// column name is the entry's `name` field, falling back to the map key.
fn columns_from(map: &serde_json::Map<String, serde_json::Value>) -> Vec<ColumnInfo> {
    map.iter()
        .map(|(key, v)| ColumnInfo {
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or(key)
                .to_string(),
            data_type: v
                .get("data_type")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            description: v
                .get("description")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        })
        .collect()
}

/// Detail for a model / seed / snapshot node.
fn detail_from_node(node: &RawNode) -> NodeDetail {
    NodeDetail {
        materialized: node.config.materialized.clone(),
        database: node.database.clone(),
        schema: node.schema.clone(),
        tags: node.tags.clone(),
        description: clean_desc(&node.description),
        columns: columns_from(&node.columns),
        original_file_path: node.original_file_path.clone(),
        package_name: node.package_name.clone(),
        access: node.access.clone(),
        group: node.group.clone(),
        contract_enforced: node.contract.as_ref().is_some_and(|c| c.enforced),
        version: node.version.clone(),
        latest_version: node.latest_version.clone(),
    }
}

/// Detail for an exposure (no materialization / warehouse coordinates; the
/// kind / owner / url live in the [`ExposureInfo`] side map instead).
fn detail_from_exposure(exposure: &RawExposure) -> NodeDetail {
    NodeDetail {
        description: clean_desc(&exposure.description),
        original_file_path: exposure.original_file_path.clone(),
        ..Default::default()
    }
}

/// Convert a wire `RawMacro` into its stored [`MacroInfo`] payload.
fn macro_info(m: &RawMacro) -> MacroInfo {
    MacroInfo {
        name: m.name.clone(),
        sql: m.macro_sql.clone(),
        depends_on: m.depends_on.macros.clone(),
    }
}

/// Detail for a source node (no materialization; it's an external table).
fn detail_from_source(source: &RawSource) -> NodeDetail {
    NodeDetail {
        materialized: None,
        database: source.database.clone(),
        schema: source.schema.clone(),
        tags: Vec::new(),
        description: clean_desc(&source.description),
        columns: columns_from(&source.columns),
        original_file_path: source.original_file_path.clone(),
        package_name: source.package_name.clone(),
        ..Default::default() // sources carry no governance fields
    }
}

/// Resolve a model NAME to its `model.<pkg>.<name>` uid — the fallback when a
/// unit_test's uid doesn't carry the standard `unit_tests.<pkg>.<model>` shape.
/// Returns the single match (a name colliding across packages can't be
/// resolved reliably anyway).
fn model_uid_by_name(nodes: &HashMap<String, NodeInfo>, name: &str) -> Option<String> {
    nodes
        .values()
        .filter(|n| n.resource_type == "model" && n.name == name)
        .map(|n| n.unique_id.clone())
        .next()
}

/// Record a test node into the per-target tests map. The target is the test's
/// `attached_node` (the node it is declared on); singular tests have none, so we
/// fall back to every node in `depends_on`. This is a pure recording — it never
/// touches the adjacency maps, so a test is never a lineage transit hop.
fn register_test(tests: &mut HashMap<String, Vec<TestInfo>>, node: &RawNode) {
    let info = TestInfo {
        name: node.name.clone(),
        kind: node
            .test_metadata
            .as_ref()
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| "singular".to_string()),
        column_name: node.column_name.clone(),
    };
    match &node.attached_node {
        Some(target) => tests.entry(target.clone()).or_default().push(info),
        None => {
            for target in &node.depends_on.nodes {
                tests.entry(target.clone()).or_default().push(info.clone());
            }
        }
    }
}

/// Prune an adjacency map (`parent_map` / `child_map`) to the kept node set.
///
/// Drops any key not in `kept`, and filters each neighbour list to kept nodes.
/// This is the "prune-first" step that makes traversal correct: an excluded
/// node can never be an endpoint or a transit hop (e.g. `model -> test -> model`
/// is broken, never silently bridged).
fn prune_adjacency(
    adjacency: &HashMap<String, Vec<String>>,
    kept: &HashSet<&String>,
) -> HashMap<String, Vec<String>> {
    adjacency
        .iter()
        .filter(|(key, _)| kept.contains(key))
        .map(|(key, neighbours)| {
            let filtered: Vec<String> = neighbours
                .iter()
                .filter(|n| kept.contains(n))
                .cloned()
                .collect();
            (key.clone(), filtered)
        })
        .collect()
}

/// Unbounded BFS transitive closure over a pruned adjacency map, excluding the
/// seed. A thin wrapper over [`closure_depth`] with no hop cap, so the traversal
/// contract (cycle-safety, dedup, seed-exclusion) has one definition.
fn closure(adjacency: &HashMap<String, Vec<String>>, start: &str) -> HashSet<String> {
    closure_depth(adjacency, start, None)
}

/// Level-by-level BFS closure limited to `max_depth` hops (`None` = unlimited),
/// excluding the seed. Cycle-safe (the `visited` set dedups). The depth cap
/// powers the lineage depth-limit view.
fn closure_depth(
    adjacency: &HashMap<String, Vec<String>>,
    start: &str,
    max_depth: Option<usize>,
) -> HashSet<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = vec![start.to_string()];
    let mut depth = 0usize;
    while !frontier.is_empty() {
        if matches!(max_depth, Some(max) if depth >= max) {
            break;
        }
        let mut next: Vec<String> = Vec::new();
        for current in &frontier {
            if let Some(neighbours) = adjacency.get(current) {
                for neighbour in neighbours {
                    if visited.insert(neighbour.clone()) {
                        next.push(neighbour.clone());
                    }
                }
            }
        }
        frontier = next;
        depth += 1;
    }
    visited.remove(start);
    visited
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RawConfig, RawTestMetadata};

    fn sample_manifest() -> RawManifest {
        // a -> b -> c, plus a test node hanging off b (must not bridge). The test
        // carries metadata so the pre-prune tests-capture is exercised too.
        let nodes = HashMap::from([
            (
                "model.p.a".to_string(),
                RawNode {
                    name: "a".into(),
                    resource_type: "model".into(),
                    path: Some("staging/a.sql".into()),
                    ..Default::default()
                },
            ),
            (
                "model.p.b".to_string(),
                RawNode {
                    name: "b".into(),
                    resource_type: "model".into(),
                    path: Some("marts/b.sql".into()),
                    config: RawConfig {
                        materialized: Some("view".into()),
                    },
                    ..Default::default()
                },
            ),
            (
                "test.p.t".to_string(),
                RawNode {
                    name: "t".into(),
                    resource_type: "test".into(),
                    attached_node: Some("model.p.b".into()),
                    column_name: Some("id".into()),
                    test_metadata: Some(RawTestMetadata {
                        name: Some("not_null".into()),
                    }),
                    ..Default::default()
                },
            ),
        ]);
        let sources = HashMap::from([(
            "source.p.s.c".to_string(),
            RawSource {
                name: "c".into(),
                resource_type: "source".into(),
                path: None,
                ..Default::default()
            },
        )]);
        // child_map: a -> b -> t (test). If we traversed raw + filtered after,
        // a -> b -> t would still stop at t; but the prune-first guarantee is
        // that t is never a transit point. Add c as an upstream of a.
        let parent_map = HashMap::from([
            ("model.p.a".to_string(), vec!["source.p.s.c".to_string()]),
            ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
            ("test.p.t".to_string(), vec!["model.p.b".to_string()]),
        ]);
        let child_map = HashMap::from([
            ("source.p.s.c".to_string(), vec!["model.p.a".to_string()]),
            ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
            ("model.p.b".to_string(), vec!["test.p.t".to_string()]),
        ]);
        RawManifest {
            metadata: Default::default(),
            nodes,
            sources,
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            parent_map,
            child_map,
        }
    }

    #[test]
    fn merge_excludes_test_and_operation() {
        let dag = Dag::build(&sample_manifest());
        // model a, model b, source c == 3 kept; test t excluded.
        assert_eq!(dag.len(), 3);
        assert!(dag.contains("model.p.a"));
        assert!(dag.contains("source.p.s.c"));
        assert!(!dag.contains("test.p.t"));
        assert_eq!(dag.count_by_resource_type("test"), 0);
    }

    #[test]
    fn analysis_nodes_are_excluded_like_tests() {
        // manifest mode is the only loader that ever sees `analysis` nodes
        // (source.rs never parses `analyses/`); excluding them here keeps the
        // two load paths' graphs identical.
        let nodes = HashMap::from([
            (
                "model.p.a".to_string(),
                RawNode {
                    name: "a".into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            ),
            (
                "analysis.p.x".to_string(),
                RawNode {
                    name: "x".into(),
                    resource_type: "analysis".into(),
                    ..Default::default()
                },
            ),
        ]);
        let parent_map =
            HashMap::from([("analysis.p.x".to_string(), vec!["model.p.a".to_string()])]);
        let child_map =
            HashMap::from([("model.p.a".to_string(), vec!["analysis.p.x".to_string()])]);
        let dag = Dag::build(&RawManifest {
            metadata: Default::default(),
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            parent_map,
            child_map,
            macros: HashMap::new(),
        });
        assert!(dag.get("analysis.p.x").is_none());
        assert_eq!(dag.count_by_resource_type("analysis"), 0);
        assert!(dag.upstream("model.p.a").is_empty());
        assert!(dag.downstream("model.p.a").is_empty());
    }

    #[test]
    fn closure_is_multihop_and_excludes_self() {
        let dag = Dag::build(&sample_manifest());
        // c -> a -> b downstream.
        let down = dag.downstream("source.p.s.c");
        assert_eq!(
            down,
            HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()])
        );
        assert!(!down.contains("source.p.s.c"));
        // b upstream is a and c.
        let up = dag.upstream("model.p.b");
        assert_eq!(
            up,
            HashSet::from(["model.p.a".to_string(), "source.p.s.c".to_string()])
        );
    }

    #[test]
    fn excluded_node_is_not_a_transit_hop() {
        let dag = Dag::build(&sample_manifest());
        // b's downstream child in raw child_map is the test node only, so after
        // exclusion downstream(b) is empty (the test is dropped, not bridged).
        assert!(dag.downstream("model.p.b").is_empty());
    }

    #[test]
    fn tests_are_captured_pre_prune_without_bridging_topology() {
        // The test node `t` is attached to model b. It must be recorded in the
        // tests side map for b, yet must NOT appear as a node nor bridge lineage:
        // downstream(b) stays empty.
        let dag = Dag::build(&sample_manifest());
        let b_tests = dag.tests("model.p.b");
        assert_eq!(b_tests.len(), 1, "b has one captured test");
        assert_eq!(b_tests[0].kind, "not_null");
        assert_eq!(b_tests[0].column_name.as_deref(), Some("id"));
        assert!(!dag.contains("test.p.t"), "test node is not a DAG node");
        assert!(
            dag.downstream("model.p.b").is_empty(),
            "test never bridges lineage"
        );
        assert!(dag.tests("model.p.a").is_empty(), "no tests on a");
    }

    #[test]
    fn detail_surfaces_materialized_from_config() {
        let dag = Dag::build(&sample_manifest());
        let b = dag.detail("model.p.b").expect("b has detail");
        assert_eq!(b.materialized.as_deref(), Some("view"));
        // A source has no materialization.
        assert_eq!(dag.detail("source.p.s.c").unwrap().materialized, None);
        // Unknown node has no detail.
        assert!(dag.detail("model.p.nope").is_none());
    }

    #[test]
    fn closure_is_cycle_safe() {
        // a <-> b cycle.
        let nodes = HashMap::from([
            (
                "model.p.a".to_string(),
                RawNode {
                    name: "a".into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            ),
            (
                "model.p.b".to_string(),
                RawNode {
                    name: "b".into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            ),
        ]);
        let child_map = HashMap::from([
            ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
            ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
        ]);
        let manifest = RawManifest {
            metadata: Default::default(),
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            parent_map: HashMap::new(),
            child_map,
            macros: HashMap::new(),
        };
        let dag = Dag::build(&manifest);
        let down = dag.downstream("model.p.a");
        // Must terminate; self excluded, b included.
        assert_eq!(down, HashSet::from(["model.p.b".to_string()]));
    }

    #[test]
    fn unknown_start_returns_empty() {
        let dag = Dag::build(&sample_manifest());
        assert!(dag.upstream("model.p.nonexistent").is_empty());
        assert!(dag.downstream("model.p.nonexistent").is_empty());
    }

    #[test]
    fn subgraph_collects_nodes_and_edges() {
        // c -> a -> b (source c, models a/b). Subgraph of `a` is {c, a, b} with
        // edges c->a and a->b.
        let dag = Dag::build(&sample_manifest());
        let sg = dag.subgraph("model.p.a");
        let ids: HashSet<&str> = sg.nodes.iter().map(|n| n.unique_id.as_str()).collect();
        assert_eq!(
            ids,
            HashSet::from(["source.p.s.c", "model.p.a", "model.p.b"]),
            "subgraph of a is its full lineage"
        );
        assert_eq!(sg.selected, "model.p.a");
        // Both dependency edges present, endpoints in the set.
        let edge_pairs: HashSet<(&str, &str)> = sg
            .edges
            .iter()
            .map(|e| (e.parent.as_str(), e.child.as_str()))
            .collect();
        assert_eq!(
            edge_pairs,
            HashSet::from([("source.p.s.c", "model.p.a"), ("model.p.a", "model.p.b")]),
            "edges are the two parent->child dependencies"
        );
    }

    #[test]
    fn subgraph_is_deterministic_and_sorted() {
        // Two calls must produce bit-identical Subgraphs (HashSet closures are
        // unordered; subgraph() must sort to a stable total order).
        let dag = Dag::build(&sample_manifest());
        let a = dag.subgraph("model.p.b");
        let b = dag.subgraph("model.p.b");
        assert_eq!(a, b, "subgraph must be deterministic across calls");
        // nodes sorted by unique_id; edges sorted by (parent, child).
        let node_ids: Vec<&str> = a.nodes.iter().map(|n| n.unique_id.as_str()).collect();
        let mut sorted = node_ids.clone();
        sorted.sort_unstable();
        assert_eq!(node_ids, sorted, "nodes sorted by unique_id");
    }

    #[test]
    fn exposures_join_as_terminator_leaves_with_side_map_payload() {
        // An exposure declared via depends_on ONLY (the source-mode shape: no
        // parent_map/child_map entries) still joins the kept set as a leaf:
        // parents from depends_on, downstream closure reaches it, and it never
        // gains children. The kind/owner/url land in the side map; a dangling
        // dep is dropped like an unresolved ref.
        use crate::manifest::{RawExposure, RawExposureOwner};
        let mut manifest = sample_manifest();
        manifest.exposures.insert(
            "exposure.p.dash".to_string(),
            RawExposure {
                name: "dash".into(),
                exposure_type: Some("dashboard".into()),
                owner: RawExposureOwner {
                    name: Some("Finance".into()),
                    email: Some("fin@example.com".into()),
                },
                url: Some("https://bi.example.com/1".into()),
                description: Some("Weekly overview.\n".into()),
                depends_on: crate::RawDependsOn {
                    nodes: vec![
                        "model.p.b".to_string(),
                        "model.p.missing".to_string(), // dangling: dropped
                    ],
                    macros: vec![],
                },
                ..Default::default()
            },
        );
        let dag = Dag::build(&manifest);
        let exp = "exposure.p.dash";
        assert_eq!(dag.count_by_resource_type("exposure"), 1);
        let info = dag.get(exp).expect("exposure is a kept node");
        assert_eq!(info.resource_type, "exposure");
        assert_eq!(
            (info.direct_up, info.direct_down),
            (1, 0),
            "leaf with 1 parent"
        );
        assert_eq!(
            dag.upstream(exp),
            HashSet::from([
                "model.p.b".to_string(),
                "model.p.a".to_string(),
                "source.p.s.c".to_string(),
            ]),
            "the dangling dep never became an edge"
        );
        assert!(dag.downstream(exp).is_empty(), "exposures have no children");
        assert!(
            dag.downstream("model.p.a").contains(exp),
            "the exposure is reachable downstream of its ancestors"
        );
        let payload = dag.exposure(exp).expect("side-map payload");
        assert_eq!(payload.exposure_type.as_deref(), Some("dashboard"));
        assert_eq!(payload.owner_name.as_deref(), Some("Finance"));
        assert_eq!(payload.owner_email.as_deref(), Some("fin@example.com"));
        assert_eq!(payload.url.as_deref(), Some("https://bi.example.com/1"));
        assert!(
            dag.exposure("model.p.b").is_none(),
            "models have no payload"
        );
        let detail = dag.detail(exp).expect("exposure detail");
        assert_eq!(detail.description.as_deref(), Some("Weekly overview."));
        assert_eq!(detail.materialized, None);
    }

    #[test]
    fn exposure_edges_from_parent_map_and_depends_on_are_deduplicated() {
        // A real manifest records exposure edges in parent_map/child_map AND in
        // depends_on; the merge must not double-count them (direct_up == 1).
        use crate::manifest::RawExposure;
        let mut manifest = sample_manifest();
        manifest.exposures.insert(
            "exposure.p.dash".to_string(),
            RawExposure {
                name: "dash".into(),
                depends_on: crate::RawDependsOn {
                    nodes: vec!["model.p.b".to_string()],
                    macros: vec![],
                },
                ..Default::default()
            },
        );
        manifest
            .parent_map
            .insert("exposure.p.dash".to_string(), vec!["model.p.b".to_string()]);
        manifest
            .child_map
            .entry("model.p.b".to_string())
            .or_default()
            .push("exposure.p.dash".to_string());
        let dag = Dag::build(&manifest);
        let info = dag.get("exposure.p.dash").unwrap();
        assert_eq!(
            info.direct_up, 1,
            "parent_map + depends_on dedup to one edge"
        );
        let b = dag.get("model.p.b").unwrap();
        assert_eq!(b.direct_down, 1, "child side deduplicated too");
    }

    #[test]
    fn subgraph_unknown_node_is_empty() {
        let dag = Dag::build(&sample_manifest());
        let sg = dag.subgraph("model.p.nope");
        assert!(sg.nodes.is_empty());
        assert!(sg.edges.is_empty());
        assert_eq!(sg.selected, "model.p.nope");
    }

    #[test]
    fn filtered_prunes_nodes_edges_and_side_maps() {
        // model.p.a -> model.p.b -> model.q.c: keep only package p.
        let mut manifest = sample_manifest();
        manifest.nodes.insert(
            "model.q.c".to_string(),
            crate::RawNode {
                name: "c".into(),
                resource_type: "model".into(),
                package_name: Some("q".into()),
                ..Default::default()
            },
        );
        manifest
            .parent_map
            .insert("model.q.c".to_string(), vec!["model.p.b".to_string()]);
        manifest
            .child_map
            .entry("model.p.b".to_string())
            .or_default()
            .push("model.q.c".to_string());
        let dag = Dag::build(&manifest);
        assert_eq!(dag.primary_package().as_deref(), Some("p"));

        let f = dag.filtered(|n| n.unique_id.starts_with("model.p."));
        assert!(f.get("model.q.c").is_none(), "foreign package dropped");
        assert!(
            f.edges()
                .iter()
                .all(|(p, c)| f.get(p).is_some() && f.get(c).is_some()),
            "no edge endpoint dangles into the pruned set"
        );
        assert!(
            f.get("model.p.b").unwrap().direct_down == 0,
            "b's dropped child no longer counts"
        );
    }

    #[test]
    fn semantic_resources_and_unit_tests_attach() {
        let mut manifest = sample_manifest();
        manifest.semantic_models.insert(
            "semantic_model.p.orders".to_string(),
            crate::manifest::RawSemanticResource {
                name: "orders".into(),
                depends_on: crate::RawDependsOn {
                    nodes: vec!["model.p.b".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        manifest.metrics.insert(
            "metric.p.order_total".to_string(),
            crate::manifest::RawSemanticResource {
                name: "order_total".into(),
                depends_on: crate::RawDependsOn {
                    nodes: vec!["semantic_model.p.orders".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        manifest.unit_tests.insert(
            "unit_tests.p.b.test_b".to_string(),
            crate::manifest::RawUnitTest {
                name: "test_b".into(),
                model: Some("b".into()),
            },
        );
        let dag = Dag::build(&manifest);
        let sem = dag.get("semantic_model.p.orders").unwrap();
        assert_eq!(sem.resource_type, "semantic_model");
        assert_eq!(sem.direct_up, 1, "semantic model hangs off model.p.b");
        let met = dag.get("metric.p.order_total").unwrap();
        assert_eq!(met.direct_up, 1, "metric chains off the semantic model");
        assert_eq!(
            dag.get("model.p.b").unwrap().direct_down,
            1,
            "semantic consumer counts as downstream"
        );
        assert!(
            dag.tests("model.p.b")
                .iter()
                .any(|t| t.kind == "unit" && t.name == "test_b"),
            "unit test recorded on its model"
        );
    }

    /// A three-package manifest where the uid-segment heuristics DISAGREE —
    /// `aaa_pkg` is the lexicographic min (1 model), `mmm_pkg` the most
    /// frequent (3 models), `zzz_proj` the recorded root project (1 model) —
    /// parameterized by `metadata.project_name` so each test picks
    /// recorded-vs-fallback. The min/most-frequent divergence matters: a
    /// fixture where they coincide could not detect the wrong one being used.
    fn multi_package_manifest(root_project: Option<&str>) -> RawManifest {
        let mut nodes = HashMap::new();
        let mut model = |uid: &str, name: &str| {
            nodes.insert(
                uid.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            );
        };
        model("model.zzz_proj.root_model", "root_model");
        model("model.aaa_pkg.pkg_a", "pkg_a");
        for i in 0..3 {
            model(&format!("model.mmm_pkg.pkg_{i}"), &format!("mmm_{i}"));
        }
        RawManifest {
            metadata: crate::manifest::RawMetadata {
                project_name: root_project.map(str::to_string),
            },
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
        }
    }

    #[test]
    fn project_name_prefers_root_project_metadata() {
        // `aaa_pkg` sorts BEFORE `zzz_proj` and `mmm_pkg` outnumbers it — but
        // metadata says the root project is `zzz_proj`, and that wins for both
        // the display name and the `--project-only` scope.
        let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
        assert_eq!(dag.project_name(), Some("zzz_proj"));
        assert_eq!(dag.primary_package().as_deref(), Some("zzz_proj"));
    }

    #[test]
    fn project_name_and_primary_package_fall_back_to_uid_heuristics() {
        // Manifests recording no `metadata.project_name` keep the legacy
        // inference — and the two heuristics must not be conflated: the
        // display name is the lexicographic min (`aaa_pkg`), the owning
        // package the most frequent (`mmm_pkg`).
        let dag = Dag::build(&multi_package_manifest(None));
        assert_eq!(dag.project_name(), Some("aaa_pkg"));
        assert_eq!(dag.primary_package().as_deref(), Some("mmm_pkg"));
    }

    #[test]
    fn blank_project_name_metadata_falls_back_too() {
        // An empty or whitespace-only `project_name` is as good as absent —
        // the fallback answers, not the blank string.
        for blank in ["", "  "] {
            let dag = Dag::build(&multi_package_manifest(Some(blank)));
            assert_eq!(dag.project_name(), Some("aaa_pkg"), "blank {blank:?}");
            assert_eq!(
                dag.primary_package().as_deref(),
                Some("mmm_pkg"),
                "blank {blank:?}"
            );
        }
    }

    #[test]
    fn primary_package_names_root_even_when_root_has_no_nodes() {
        // A deps-only manifest records a root project that contributes NO
        // nodes: `primary_package` still names it honestly (it is a fact
        // about the manifest, not a vote over surviving uids) — and a
        // `--project-only` filter therefore yields an EMPTY graph, which
        // `cli.rs` surfaces as a warning rather than silently re-scoping to a
        // foreign package.
        let mut manifest = multi_package_manifest(Some("zzz_proj"));
        manifest.nodes.remove("model.zzz_proj.root_model");
        let dag = Dag::build(&manifest);
        assert_eq!(dag.primary_package().as_deref(), Some("zzz_proj"));
        assert!(dag
            .filtered(|n| n.unique_id.split('.').nth(1) == Some("zzz_proj"))
            .is_empty());
    }

    #[test]
    fn filtered_keeps_the_root_project_name() {
        // `--project-only` filters to the root package; the filtered Dag must
        // still name itself `zzz_proj` (metadata is a property of the source
        // manifest, not of the surviving uid set).
        let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
        let f = dag.filtered(|n| n.unique_id.split('.').nth(1) == Some("zzz_proj"));
        assert_eq!(f.len(), 1);
        assert_eq!(f.project_name(), Some("zzz_proj"));
        assert_eq!(f.primary_package().as_deref(), Some("zzz_proj"));
    }

    // ---- compiled side map ingestion (target/compiled/) -------------------
    // `Dag::ingest_compiled` driven straight on tempdirs; the wider
    // load-path coverage lives in tests/compiled_fixture.rs.

    fn stamp_mtime(path: &Path, secs_ago: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn ingest_compiled_installed_package_resolves_only_package_dirs() {
        // An installed-package node's original_file_path is PACKAGE-root-
        // relative: its source lives under `dbt_packages/<pkg>/` or
        // `dbt_internal_packages/<pkg>/` — never `<project_root>/<ofp>`. A
        // local file sharing the path must not influence staleness.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let compiled_dir = root.join("target/compiled");
        let cfile = compiled_dir.join("dep/models/m.sql");
        std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
        std::fs::write(&cfile, "select 1").unwrap();
        stamp_mtime(&cfile, 3600); // expansion is old

        // An unrelated project file at <root>/models/m.sql, NEWER than the
        // expansion: a local node would read it as the source; `dep` must not.
        std::fs::create_dir_all(root.join("models")).unwrap();
        let local_src = root.join("models/m.sql");
        std::fs::write(&local_src, "select unrelated").unwrap();
        stamp_mtime(&local_src, 60);

        let mut manifest = sample_manifest();
        manifest.nodes.insert(
            "model.dep.m".to_string(),
            crate::RawNode {
                name: "m".into(),
                resource_type: "model".into(),
                package_name: Some("dep".into()),
                original_file_path: Some("models/m.sql".into()),
                ..Default::default()
            },
        );
        let mut dag = Dag::build(&manifest);
        dag.ingest_compiled(&compiled_dir, Some(root));
        let c = dag
            .compiled("model.dep.m")
            .expect("compiled file resolves under dep/");
        assert!(
            !c.stale,
            "a project-root file must not mark an installed package stale"
        );

        // The package checkout IS the real source — newer means stale.
        // (ingest skips uids already in the map, so rebuild for round two.)
        let pkg_src = root.join("dbt_packages/dep/models/m.sql");
        std::fs::create_dir_all(pkg_src.parent().unwrap()).unwrap();
        std::fs::write(&pkg_src, "select 2").unwrap();
        stamp_mtime(&pkg_src, 30);
        let mut dag = Dag::build(&manifest);
        dag.ingest_compiled(&compiled_dir, Some(root));
        assert!(
            dag.compiled("model.dep.m").unwrap().stale,
            "dbt_packages source newer than expansion is stale"
        );
    }

    #[test]
    fn ingest_compiled_loads_python_models() {
        // dbt keeps a `.py` artifact under compiled/ for Python models
        // (Snowpark/Databricks) — same <pkg>/<ofp> layout, different suffix.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let compiled_dir = root.join("target/compiled");
        let cfile = compiled_dir.join("p/models/py_mod.py");
        std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
        std::fs::write(&cfile, "def model(dbt, session):\n    return dbt.ref('a')").unwrap();

        let mut manifest = sample_manifest();
        manifest.nodes.insert(
            "model.p.py_mod".to_string(),
            crate::RawNode {
                name: "py_mod".into(),
                resource_type: "model".into(),
                package_name: Some("p".into()),
                original_file_path: Some("models/py_mod.py".into()),
                ..Default::default()
            },
        );
        let mut dag = Dag::build(&manifest);
        dag.ingest_compiled(&compiled_dir, Some(root));
        assert_eq!(
            dag.compiled_code("model.p.py_mod"),
            Some("def model(dbt, session):\n    return dbt.ref('a')"),
            "the .py suffix must not exclude Python models"
        );
    }

    #[test]
    fn ingest_compiled_empty_package_name_uses_uid_segment() {
        // `package_name: ""` never occurs in real dbt output, but a
        // hand-rolled manifest could carry one — an empty string must fall
        // through to the uid's project segment like a missing field.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let compiled_dir = root.join("target/compiled");
        let cfile = compiled_dir.join("p/models/e.sql");
        std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
        std::fs::write(&cfile, "select 3").unwrap();

        let mut manifest = sample_manifest();
        manifest.nodes.insert(
            "model.p.e".to_string(),
            crate::RawNode {
                name: "e".into(),
                resource_type: "model".into(),
                package_name: Some(String::new()),
                original_file_path: Some("models/e.sql".into()),
                ..Default::default()
            },
        );
        let mut dag = Dag::build(&manifest);
        dag.ingest_compiled(&compiled_dir, Some(root));
        assert_eq!(dag.compiled_code("model.p.e"), Some("select 3"));
    }
}
