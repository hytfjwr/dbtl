//! dbtl core library: parse a dbt `manifest.json` (minimal subset), merge
//! `nodes` (excluding `test` / `operation`) and `sources` into a single
//! `unique_id -> NodeInfo` map, build a pruned DAG over the kept node set,
//! and compute upstream / downstream transitive closures (cycle-safe).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub mod action;
pub mod app;
pub mod effect;
pub mod layout;
pub mod model_list;
pub mod source;
pub mod ui;

pub use action::{
    dispatch, help_lines, Action, Direction, HelpLine, Mode, ModeKind, SearchTarget, SqlView,
    StatsView,
};
pub use app::{apply_action, layer_violation_edges, App, AppStats, ListFilter, Outcome};
pub use effect::Effect;
pub use layout::{
    anchor_offset, blit, clamp_offset, layout, layout_mode, CellAttr, CharGrid, GlyphMode, Layout,
    LensTint, MaterializationClass, NodeRect,
};
pub use model_list::{
    build_filtered_model_list, build_model_list, match_indices, name_matches_query, DisplayRow,
    ModelGroup, ModelList, SortMode, LAYER_ORDER,
};
pub use source::{load_dag_from_source, manifest_from_source};
pub use ui::{
    draw, handle_key, reduce_selection, Focus, KeyOutcome, LineageLens, RenderCtx, UiState,
};

/// `config` sub-object of a node (only the fields we surface).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawConfig {
    #[serde(default)]
    pub materialized: Option<String>,
}

/// `depends_on` sub-object of a node (the node refs it points at).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawDependsOn {
    #[serde(default)]
    pub nodes: Vec<String>,
}

/// `test_metadata` sub-object of a generic test node (`name` is the test kind:
/// `unique`, `not_null`, `relationships`, …). Absent for singular tests.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawTestMetadata {
    #[serde(default)]
    pub name: Option<String>,
}

/// A single node in the parsed manifest (`nodes` entry).
///
/// Only the subset of fields we surface is typed; unknown fields are ignored.
/// Every node must deserialize (exclusion happens *after* parsing), so anything
/// not guaranteed across all resource types is optional. `Default` is derived so
/// the unit-test literals can use `..Default::default()` and never churn again as
/// fields are added. `columns` keeps dbt definition order via `serde_json`'s
/// `preserve_order` (its `Map` is then an `IndexMap`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawNode {
    pub name: String,
    pub resource_type: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub config: RawConfig,
    #[serde(default)]
    pub columns: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    // --- test-only fields (captured pre-prune for the tests side map) ---
    #[serde(default)]
    pub test_metadata: Option<RawTestMetadata>,
    #[serde(default)]
    pub attached_node: Option<String>,
    #[serde(default)]
    pub column_name: Option<String>,
    #[serde(default)]
    pub depends_on: RawDependsOn,
    /// The node's raw (uncompiled) SQL — the real dbt manifest field name (v12
    /// nodes carry it). `#[serde(default)]` + `Option` so test literals using
    /// `..Default::default()` never churn; sources/seeds carry none.
    #[serde(default)]
    pub raw_code: Option<String>,
}

/// A single source in the parsed manifest (`sources` entry).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawSource {
    pub name: String,
    pub resource_type: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub columns: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
}

/// Minimal subset of the dbt manifest we care about.
///
/// `deny_unknown_fields` is intentionally NOT set: the v12 schema has many
/// fields we don't model, and they must be ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct RawManifest {
    pub nodes: HashMap<String, RawNode>,
    pub sources: HashMap<String, RawSource>,
    pub parent_map: HashMap<String, Vec<String>>,
    pub child_map: HashMap<String, Vec<String>>,
}

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
/// - `nodes`: unified `unique_id -> NodeInfo` map (model/source/seed/snapshot only).
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
}

/// Resource types that are excluded from the unified map and the DAG.
fn is_excluded(resource_type: &str) -> bool {
    resource_type == "test" || resource_type == "operation"
}

/// Read and parse a manifest from disk.
///
/// Returns `Err` (never panics) when the file is missing or the contents are
/// not valid JSON / do not match the expected manifest shape. The error
/// message names the cause (file read vs JSON parse).
pub fn load_manifest<P: AsRef<Path>>(path: P) -> Result<RawManifest> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest file: {}", path.display()))?;
    let manifest: RawManifest = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse manifest JSON: {}", path.display()))?;
    Ok(manifest)
}

/// Convenience: load a manifest from disk and build its DAG in one call.
pub fn load_dag<P: AsRef<Path>>(path: P) -> Result<Dag> {
    let manifest = load_manifest(path)?;
    Ok(Dag::build(&manifest))
}

impl Dag {
    /// Build the DAG from a parsed manifest.
    ///
    /// Excludes `test` / `operation` nodes, merges `nodes` + `sources` into a
    /// single map, and prunes `parent_map` / `child_map` down to the kept set
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

        for (unique_id, node) in &manifest.nodes {
            if node.resource_type == "test" {
                register_test(&mut tests, node); // capture pre-prune, then skip
                continue;
            }
            if is_excluded(&node.resource_type) {
                continue;
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
        let parents = prune_adjacency(&manifest.parent_map, &kept);
        let children = prune_adjacency(&manifest.child_map, &kept);

        // Stamp each node's direct (1-hop) parent/child counts from the pruned
        // adjacency (list-pane dependency badges + orphan detection) and its
        // tests count (the lineage box's `tests:N` label).
        for (uid, info) in nodes.iter_mut() {
            info.direct_up = parents.get(uid).map_or(0, |v| v.len());
            info.direct_down = children.get(uid).map_or(0, |v| v.len());
            info.test_count = tests.get(uid).map_or(0, |v| v.len());
        }

        Dag {
            nodes,
            parents,
            children,
            details,
            tests,
            sql,
        }
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

    /// The detail payload (materialized / schema / columns / …) for a node, or
    /// `None` if unknown. Used by the structure modal and the status line.
    pub fn detail(&self, unique_id: &str) -> Option<&NodeDetail> {
        self.details.get(unique_id)
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
    }
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
            nodes,
            sources,
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
            nodes,
            sources: HashMap::new(),
            parent_map: HashMap::new(),
            child_map,
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
    fn subgraph_unknown_node_is_empty() {
        let dag = Dag::build(&sample_manifest());
        let sg = dag.subgraph("model.p.nope");
        assert!(sg.nodes.is_empty());
        assert!(sg.edges.is_empty());
        assert_eq!(sg.selected, "model.p.nope");
    }
}
