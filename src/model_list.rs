//! Model-list assembly: group the DAG's `model` nodes by the first directory
//! of their `path`, in dbt's fixed logical layer order, and sort each group by
//! `name`.
//!
//! This is the data source for the left pane. It is deliberately split from
//! drawing so it can be unit-tested headlessly against the committed fixture.
//!
//! Two views are produced:
//! - `models`: the flat, display-order list of the 45 selectable models. The
//!   selection index `0..len` indexes directly into this (group headers are
//!   *not* selectable; the cursor skips them).
//! - `rows`: the display rows (group headers interleaved with model rows),
//!   used by the renderer. Each model row carries its index into `models` so
//!   the renderer can highlight the selected one without recomputing.

use crate::{Dag, NodeInfo};

/// dbt layer order. Load-bearing and fixed: this is the logical layer order,
/// NOT alphabetical (alphabetical would be intermediate/marts/staging/utilities,
/// which is wrong). Groups are emitted in exactly this order; any unexpected
/// fifth top-level directory is appended after these, so we never panic.
pub const LAYER_ORDER: [&str; 4] = ["staging", "intermediate", "marts", "utilities"];

/// How models are ordered WITHIN each layer group. Groups themselves are always
/// in [`LAYER_ORDER`]; `SortMode` only reorders the models inside a group.
///
/// [`SortMode::Layer`] (the default) reproduces the original behaviour exactly:
/// name ascending, tie-broken by `unique_id`. The other modes add a primary key
/// (a `NodeInfo` field, descending) before that same name/unique_id tie-break,
/// so every mode is fully deterministic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortMode {
    /// Today's behaviour: name ascending, then `unique_id`.
    #[default]
    Layer,
    /// By direct downstream count (the `↓N` badge) descending, then name/unique_id.
    Downstream,
    /// By `test_count` descending, then name/unique_id.
    Tests,
}

impl SortMode {
    /// The next sort mode in the `.`-cycle: Layer → Downstream → Tests → Layer.
    pub fn next(self) -> SortMode {
        match self {
            SortMode::Layer => SortMode::Downstream,
            SortMode::Downstream => SortMode::Tests,
            SortMode::Tests => SortMode::Layer,
        }
    }

    /// A short label for the status bar (pure ASCII).
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Layer => "layer",
            SortMode::Downstream => "downstream",
            SortMode::Tests => "tests",
        }
    }
}

/// A single layer group in the model list (header + its models in `name` order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGroup {
    /// First-directory key, e.g. `"staging"`.
    pub layer: String,
    /// Models in this group, sorted by `name` ascending.
    pub models: Vec<NodeInfo>,
}

/// A display row in the left pane: either a group header or a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayRow {
    /// A group header. Carries the layer name and the model count in the group.
    Header { layer: String, count: usize },
    /// A model row. `model_index` is the position in [`ModelList::models`]
    /// (i.e. the selection space `0..len`).
    Model { model_index: usize, name: String },
}

/// The assembled model list: ordered groups, the flat selectable model list,
/// and the interleaved display rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelList {
    /// Layer groups in fixed logical order.
    pub groups: Vec<ModelGroup>,
    /// Flat, display-order list of selectable models (selection space `0..len`).
    pub models: Vec<NodeInfo>,
    /// Interleaved header/model rows for rendering.
    pub rows: Vec<DisplayRow>,
    /// Maps a model selection index (`0..len`) to its position in [`rows`]
    /// (the display-row index). This is the bridge between the model-space
    /// selection and the display-row-space scrolling/rendering: the renderer and
    /// the scroll-follow logic both measure against `rows`, so the selected
    /// model's *display row* (not its model index) is what must stay visible.
    ///
    /// [`rows`]: ModelList::rows
    model_to_row: Vec<usize>,
}

impl ModelList {
    /// Number of selectable models (the selection space is `0..len`).
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether there are no selectable models.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// The model at the given selection index, if in range.
    pub fn model_at(&self, index: usize) -> Option<&NodeInfo> {
        self.models.get(index)
    }

    /// Total number of display rows (headers + models). This is the size of the
    /// display-row space that scrolling operates in.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The model selection index at display-row `row`, or `None` if that row is a
    /// header or out of range. The inverse of [`row_of_model`](ModelList::row_of_model),
    /// used to turn a list-pane mouse click into a selection.
    pub fn model_at_row(&self, row: usize) -> Option<usize> {
        match self.rows.get(row) {
            Some(DisplayRow::Model { model_index, .. }) => Some(*model_index),
            _ => None,
        }
    }

    /// The display-row index of the given model selection index.
    ///
    /// Because each group emits one header row before its models, a model's
    /// display row is always strictly greater than its model index. Out-of-range
    /// indices (e.g. an empty list) return 0 so callers never panic.
    pub fn row_of_model(&self, model_index: usize) -> usize {
        self.model_to_row.get(model_index).copied().unwrap_or(0)
    }

    /// The topmost display row that should be revealed when this model becomes
    /// selected: the model's own row, except for a *first-in-group* model, where
    /// it is the preceding group-header row so the header scrolls into view with
    /// it.
    ///
    /// This is the scroll-UP anchor (see [`UiState::ensure_visible`]). Scrolling
    /// down still anchors on [`row_of_model`] so the model itself is never
    /// clipped. For model 0 this yields row 0, which keeps the documented
    /// "selection at index 0 ⇒ offset 0 (top)" property.
    ///
    /// [`row_of_model`]: ModelList::row_of_model
    pub fn reveal_row_of_model(&self, model_index: usize) -> usize {
        let row = self.row_of_model(model_index);
        if row > 0 && matches!(self.rows.get(row - 1), Some(DisplayRow::Header { .. })) {
            row - 1
        } else {
            row
        }
    }
}

/// First directory of a `path` (the part before the first `/`).
///
/// Returns `None` when the node has no `path` (sources/seeds/snapshots), so
/// only models — which always carry a `path` — contribute a layer. `pub` so the
/// lineage layer + layer-violation lenses classify a node by the SAME rule the
/// list grouping uses (no duplicated taxonomy).
pub fn first_dir(node: &NodeInfo) -> Option<&str> {
    node.path
        .as_deref()
        .map(|p| p.split('/').next().unwrap_or(p))
}

/// Rank a layer for ordering: known layers get their fixed index ([`LAYER_ORDER`]
/// position); any unknown layer is sorted after all known ones (and never panics).
/// `pub` so the layer-violation lens can compare two models' ranks (a backward
/// edge is `rank(parent) > rank(child)`) using the SAME order the list groups by.
pub fn layer_rank(layer: &str) -> usize {
    LAYER_ORDER
        .iter()
        .position(|&l| l == layer)
        .unwrap_or(LAYER_ORDER.len())
}

/// Build the model list from a [`Dag`].
///
/// - Collects only `resource_type == "model"` nodes (sources/seeds/snapshots
///   are not primary list entries; they appear only in lineage).
/// - Groups by the first directory of `path`.
/// - Orders groups by [`LAYER_ORDER`] (unknown layers appended, by name).
/// - Sorts each group's models per `sort` ([`SortMode`]): always a name/unique_id
///   tie-break, optionally preceded by a descending field key. `SortMode::Layer`
///   is byte-identical to the original name-then-unique_id ordering.
/// - Produces the flat selectable list and interleaved display rows.
pub fn build_model_list(dag: &Dag, sort: SortMode) -> ModelList {
    // Collect models grouped by layer key.
    let mut by_layer: Vec<(String, Vec<NodeInfo>)> = Vec::new();
    for node in dag.nodes().values() {
        if node.resource_type != "model" {
            continue;
        }
        // Models without a path would be unexpected; bucket them under "" so we
        // neither drop nor panic on them.
        let layer = first_dir(node).unwrap_or("").to_string();
        match by_layer.iter_mut().find(|(l, _)| *l == layer) {
            Some((_, models)) => models.push(node.clone()),
            None => by_layer.push((layer, vec![node.clone()])),
        }
    }

    // Order groups by fixed logical layer order; unknown layers after, by name.
    by_layer.sort_by(|(a, _), (b, _)| layer_rank(a).cmp(&layer_rank(b)).then_with(|| a.cmp(b)));

    // Sort within each group per `sort`. The primary key is mode-specific (and
    // descending for the field modes); the name/unique_id tie-break is shared, so
    // `SortMode::Layer` (primary == Equal) collapses to the original ordering and
    // every mode stays deterministic.
    for (_, models) in &mut by_layer {
        models.sort_by(|a, b| {
            let primary = match sort {
                SortMode::Layer => std::cmp::Ordering::Equal,
                SortMode::Downstream => b.direct_down.cmp(&a.direct_down), // DESC
                SortMode::Tests => b.test_count.cmp(&a.test_count),        // DESC
            };
            primary
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.unique_id.cmp(&b.unique_id))
        });
    }

    assemble_model_list(by_layer)
}

/// Materialize a [`ModelList`] from layer-ordered groups: build the flat `models`
/// list, the `groups`, the interleaved `rows`, and the model-index -> display-row
/// map in lockstep (so scrolling measures against rendered rows, not the model
/// count). This is the SINGLE place the model-space / display-row-space bridge is
/// constructed, so the full and filtered builders cannot drift apart (the
/// coupling that, if broken, reintroduces the clip bug).
fn assemble_model_list(by_layer: Vec<(String, Vec<NodeInfo>)>) -> ModelList {
    let mut groups = Vec::with_capacity(by_layer.len());
    let mut models = Vec::new();
    let mut rows = Vec::new();
    let mut model_to_row = Vec::new();
    for (layer, group_models) in by_layer {
        rows.push(DisplayRow::Header {
            layer: layer.clone(),
            count: group_models.len(),
        });
        for node in &group_models {
            model_to_row.push(rows.len()); // this model's display-row index
            rows.push(DisplayRow::Model {
                model_index: models.len(),
                name: node.name.clone(),
            });
            models.push(node.clone());
        }
        groups.push(ModelGroup {
            layer,
            models: group_models,
        });
    }

    ModelList {
        groups,
        models,
        rows,
        model_to_row,
    }
}

/// Case-insensitive subsequence match (fzf-style): every char of `query` appears
/// in `name` in order. An empty query matches everything. Whitespace in the
/// query is ignored. Names are ASCII in practice, so ASCII lowercasing suffices.
/// Shared by the list filter and the lineage node-jump search.
pub fn name_matches_query(name: &str, query: &str) -> bool {
    let mut q = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .peekable();
    if q.peek().is_none() {
        return true; // empty query
    }
    for nc in name.chars().map(|c| c.to_ascii_lowercase()) {
        match q.peek() {
            Some(&qc) if qc == nc => {
                q.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    q.peek().is_none()
}

/// The char positions in `name` consumed by a successful subsequence match of
/// `query`, using the SAME case-insensitive, whitespace-ignoring rule as
/// [`name_matches_query`] — so the render-time highlight marks exactly the chars
/// the filter accepted. Returns an empty vec when the query is empty or `name`
/// does not match (an incomplete subsequence is NOT a match).
///
/// Indices are CHAR positions (not byte offsets); list names are ASCII in
/// practice, so callers may treat them as byte positions, but a multibyte name
/// must be split with `.chars().enumerate()` to stay on char boundaries.
pub fn match_indices(name: &str, query: &str) -> Vec<usize> {
    let mut q = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .peekable();
    if q.peek().is_none() {
        return Vec::new(); // empty query highlights nothing
    }
    let mut hits = Vec::new();
    for (i, nc) in name.chars().enumerate() {
        let nc = nc.to_ascii_lowercase();
        if q.peek() == Some(&nc) {
            hits.push(i);
            q.next();
            if q.peek().is_none() {
                break;
            }
        }
    }
    if q.peek().is_some() {
        return Vec::new(); // incomplete ⇒ not a match
    }
    hits
}

/// Build a FILTERED model list keeping only models whose name matches `query`
/// (subsequence), dropping now-empty groups, and **recomputing** `rows` /
/// `model_to_row` / `row_count` against the filtered sequence.
///
/// This is the load-bearing piece for search: it returns a fully self-consistent
/// [`ModelList`] so the renderer and `ensure_visible` still measure against the
/// exact same row sequence (the two-coordinate-space invariant). Selection is
/// re-resolved by `unique_id` at the call site, never carried as a raw index
/// across a filter change.
///
/// It takes no [`SortMode`]: the filter preserves the order of `full.groups[].models`
/// (the `.filter(...).cloned()` loop keeps their sequence), so it INHERITS whatever
/// sort `full` was built with — re-running it after a sort change reorders the
/// filtered view for free.
pub fn build_filtered_model_list(full: &ModelList, query: &str) -> ModelList {
    filter_model_list(full, |m| crate::name_matches_query(&m.name, query))
}

/// Narrow a [`ModelList`] to the models `keep` accepts, preserving the group
/// order and per-group model order of `full` and dropping emptied groups (so
/// headers never dangle). The general predicate form behind the name-query
/// search filter, shared by the persistent untested / bookmarked list filters.
pub fn filter_model_list(full: &ModelList, keep: impl Fn(&NodeInfo) -> bool) -> ModelList {
    let mut matched_groups: Vec<(String, Vec<NodeInfo>)> = Vec::new();
    for group in &full.groups {
        let matched: Vec<NodeInfo> = group.models.iter().filter(|m| keep(m)).cloned().collect();
        if matched.is_empty() {
            continue;
        }
        matched_groups.push((group.layer.clone(), matched));
    }
    assemble_model_list(matched_groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;

    /// A tiny synthetic manifest with 2 models per layer, plus a non-model node
    /// that must be excluded from the list. Order of insertion is intentionally
    /// scrambled to prove the assembly sorts/orders rather than preserving it.
    fn synthetic() -> Dag {
        let mut nodes = HashMap::new();
        let mut add = |id: &str, name: &str, path: &str| {
            nodes.insert(
                id.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    ..Default::default()
                },
            );
        };
        // Scrambled insertion order across layers and within layers.
        add("model.p.m_b", "marts_b", "marts/m_b.sql");
        add("model.p.u_a", "util_a", "utilities/u_a.sql");
        add("model.p.s_b", "stg_b", "staging/s_b.sql");
        add("model.p.i_a", "int_a", "intermediate/i_a.sql");
        add("model.p.s_a", "stg_a", "staging/sub/s_a.sql");
        add("model.p.m_a", "marts_a", "marts/m_a.sql");
        add("model.p.u_b", "util_b", "utilities/u_b.sql");
        add("model.p.i_b", "int_b", "intermediate/i_b.sql");
        // A source must not appear in the list.
        nodes.insert(
            "model.p.skip_seed".to_string(),
            RawNode {
                name: "a_seed".into(),
                resource_type: "seed".into(),
                path: Some("seeds/x.csv".into()),
                ..Default::default()
            },
        );
        let manifest = RawManifest {
            nodes,
            sources: HashMap::new(),
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
        };
        Dag::build(&manifest)
    }

    #[test]
    fn groups_are_in_fixed_logical_order_not_alphabetical() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        let layers: Vec<&str> = list.groups.iter().map(|g| g.layer.as_str()).collect();
        assert_eq!(
            layers,
            vec!["staging", "intermediate", "marts", "utilities"],
            "groups must follow dbt logical order, not alphabetical"
        );
    }

    #[test]
    fn non_model_nodes_are_excluded() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        assert_eq!(list.len(), 8, "only the 8 models, seed excluded");
        assert!(
            list.models.iter().all(|m| m.resource_type == "model"),
            "list must contain only models"
        );
    }

    #[test]
    fn within_group_sorted_by_name() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        for group in &list.groups {
            let names: Vec<&str> = group.models.iter().map(|m| m.name.as_str()).collect();
            let mut sorted = names.clone();
            sorted.sort_unstable();
            assert_eq!(names, sorted, "group {} not name-sorted", group.layer);
        }
    }

    #[test]
    fn flat_index_follows_display_order() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        let names: Vec<&str> = list.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["stg_a", "stg_b", "int_a", "int_b", "marts_a", "marts_b", "util_a", "util_b"]
        );
    }

    #[test]
    fn rows_interleave_headers_and_models_with_back_indices() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        // First row is the staging header.
        assert_eq!(
            list.rows[0],
            DisplayRow::Header {
                layer: "staging".into(),
                count: 2
            }
        );
        // The model rows' back-indices must enumerate 0..len in order.
        let model_indices: Vec<usize> = list
            .rows
            .iter()
            .filter_map(|r| match r {
                DisplayRow::Model { model_index, .. } => Some(*model_index),
                _ => None,
            })
            .collect();
        assert_eq!(model_indices, (0..list.len()).collect::<Vec<_>>());
    }

    #[test]
    fn row_of_model_points_at_the_model_row() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        // For every model, row_of_model must land exactly on the matching
        // DisplayRow::Model (this is the bridge scrolling measures against).
        for i in 0..list.len() {
            let r = list.row_of_model(i);
            assert!(r > 0, "a model row is never the very first (a header is)");
            match &list.rows[r] {
                DisplayRow::Model { model_index, .. } => {
                    assert_eq!(*model_index, i, "row_of_model({i}) must map to model {i}")
                }
                other => panic!("row_of_model({i}) -> {other:?}, expected a Model row"),
            }
        }
        // Concretely: 2 models/layer with a header before each group, so the
        // first model of each group sits at rows 1, 4, 7, 10.
        assert_eq!(
            list.row_of_model(0),
            1,
            "first staging model after its header"
        );
        assert_eq!(list.row_of_model(2), 4, "first intermediate model");
        assert_eq!(list.row_of_model(4), 7, "first marts model");
        assert_eq!(list.row_of_model(6), 10, "first utilities model");
    }

    #[test]
    fn model_at_row_inverts_row_of_model_and_skips_headers() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        // Every model row maps back to its index; the inverse of row_of_model.
        for i in 0..list.len() {
            assert_eq!(list.model_at_row(list.row_of_model(i)), Some(i));
        }
        // Row 0 is the staging header — not a model.
        assert_eq!(list.model_at_row(0), None, "a header row has no model");
        // Out-of-range row is safe.
        assert_eq!(list.model_at_row(9999), None);
    }

    #[test]
    fn row_of_model_out_of_range_is_zero_not_panic() {
        let list = build_model_list(&synthetic(), SortMode::Layer);
        assert_eq!(list.row_of_model(999), 0, "out-of-range index is safe");
    }

    #[test]
    fn filter_keeps_matches_drops_empty_groups_and_rebuilds_rows() {
        let full = build_model_list(&synthetic(), SortMode::Layer);
        // Subsequence "stga" matches stg_a (s,t,g,_,a) but no other model; only
        // the staging group survives.
        let filtered = build_filtered_model_list(&full, "stga");
        assert_eq!(filtered.len(), 1, "only stg_a matches 'stga'");
        assert_eq!(filtered.models[0].name, "stg_a");
        let layers: Vec<&str> = filtered.groups.iter().map(|g| g.layer.as_str()).collect();
        assert_eq!(layers, vec!["staging"], "empty groups are dropped");

        // Rows/model_to_row stay self-consistent: every model row maps back to
        // its own index (the two-coordinate-space bridge must hold post-filter).
        for i in 0..filtered.len() {
            let r = filtered.row_of_model(i);
            match &filtered.rows[r] {
                DisplayRow::Model { model_index, .. } => assert_eq!(*model_index, i),
                other => panic!("row_of_model({i}) -> {other:?}"),
            }
        }
    }

    #[test]
    fn filter_empty_query_keeps_everything() {
        let full = build_model_list(&synthetic(), SortMode::Layer);
        let filtered = build_filtered_model_list(&full, "");
        assert_eq!(filtered.len(), full.len(), "empty query keeps all models");
        assert_eq!(filtered.row_count(), full.row_count(), "and all rows");
    }

    #[test]
    fn filter_is_case_insensitive_subsequence() {
        let full = build_model_list(&synthetic(), SortMode::Layer);
        // "UA" (uppercase, subsequence) matches util_a.
        let filtered = build_filtered_model_list(&full, "UA");
        assert!(filtered.models.iter().any(|m| m.name == "util_a"));
        // A query no model satisfies yields an empty list (no panic, no groups).
        let none = build_filtered_model_list(&full, "zzzz");
        assert_eq!(none.len(), 0);
        assert_eq!(none.row_count(), 0);
        assert!(none.groups.is_empty());
    }

    #[test]
    fn unknown_layer_is_appended_not_panicked() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "model.p.x".to_string(),
            RawNode {
                name: "x".into(),
                resource_type: "model".into(),
                path: Some("staging/x.sql".into()),
                ..Default::default()
            },
        );
        nodes.insert(
            "model.p.z".to_string(),
            RawNode {
                name: "z".into(),
                resource_type: "model".into(),
                path: Some("experimental/z.sql".into()),
                ..Default::default()
            },
        );
        let manifest = RawManifest {
            nodes,
            sources: HashMap::new(),
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
        };
        let list = build_model_list(&Dag::build(&manifest), SortMode::Layer);
        let layers: Vec<&str> = list.groups.iter().map(|g| g.layer.as_str()).collect();
        assert_eq!(
            layers,
            vec!["staging", "experimental"],
            "unknown layer last"
        );
    }

    /// A staging group with *intra-group variation* so the sort modes actually
    /// reorder: `stg_a` has 1 downstream + 0 tests, `stg_b` has 2 downstream +
    /// 1 test. Name-ascending (Layer) gives [stg_a, stg_b]; both field modes
    /// (DESC) give [stg_b, stg_a]. Two intermediate children give the downstream
    /// counts; a `test` node attached to `stg_b` gives its test count.
    fn sort_fixture() -> Dag {
        let mut nodes = HashMap::new();
        let mut add = |id: &str, name: &str, path: &str| {
            nodes.insert(
                id.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    ..Default::default()
                },
            );
        };
        add("model.p.s_a", "stg_a", "staging/s_a.sql");
        add("model.p.s_b", "stg_b", "staging/s_b.sql");
        add("model.p.i_x", "int_x", "intermediate/i_x.sql");
        add("model.p.i_y", "int_y", "intermediate/i_y.sql");
        // A generic test attached to stg_b (captured pre-prune → test_count 1).
        nodes.insert(
            "test.p.not_null_stg_b".to_string(),
            RawNode {
                name: "not_null_stg_b".into(),
                resource_type: "test".into(),
                attached_node: Some("model.p.s_b".into()),
                test_metadata: Some(crate::RawTestMetadata {
                    name: Some("not_null".into()),
                }),
                ..Default::default()
            },
        );
        // child_map: stg_a → {int_x}; stg_b → {int_x, int_y}. So direct_down is
        // stg_a:1, stg_b:2. parent_map mirrors it (kept consistent for Dag::build).
        let mut child_map = HashMap::new();
        child_map.insert("model.p.s_a".to_string(), vec!["model.p.i_x".to_string()]);
        child_map.insert(
            "model.p.s_b".to_string(),
            vec!["model.p.i_x".to_string(), "model.p.i_y".to_string()],
        );
        let mut parent_map = HashMap::new();
        parent_map.insert(
            "model.p.i_x".to_string(),
            vec!["model.p.s_a".to_string(), "model.p.s_b".to_string()],
        );
        parent_map.insert("model.p.i_y".to_string(), vec!["model.p.s_b".to_string()]);
        let manifest = RawManifest {
            nodes,
            sources: HashMap::new(),
            parent_map,
            child_map,
        };
        Dag::build(&manifest)
    }

    /// The staging group's model names in a built list, for a sort-order assert.
    fn staging_names(list: &ModelList) -> Vec<String> {
        list.groups
            .iter()
            .find(|g| g.layer == "staging")
            .map(|g| g.models.iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn sort_layer_is_name_ascending_within_group() {
        // Confirm the fixture's counts (so the other asserts are meaningful) and
        // that Layer mode ignores them entirely: pure name order.
        let dag = sort_fixture();
        assert_eq!(dag.get("model.p.s_a").unwrap().direct_down, 1);
        assert_eq!(dag.get("model.p.s_b").unwrap().direct_down, 2);
        assert_eq!(dag.get("model.p.s_a").unwrap().test_count, 0);
        assert_eq!(dag.get("model.p.s_b").unwrap().test_count, 1);
        let list = build_model_list(&dag, SortMode::Layer);
        assert_eq!(staging_names(&list), vec!["stg_a", "stg_b"]);
    }

    #[test]
    fn sort_downstream_orders_by_direct_down_desc() {
        let list = build_model_list(&sort_fixture(), SortMode::Downstream);
        // stg_b (2 down) before stg_a (1 down) — reversed from name order.
        assert_eq!(staging_names(&list), vec!["stg_b", "stg_a"]);
    }

    #[test]
    fn sort_tests_orders_by_test_count_desc() {
        let list = build_model_list(&sort_fixture(), SortMode::Tests);
        // stg_b (1 test) before stg_a (0 tests) — reversed from name order.
        assert_eq!(staging_names(&list), vec!["stg_b", "stg_a"]);
    }

    #[test]
    fn sort_modes_never_change_group_order() {
        // Whatever the sort, the layer GROUPS stay in fixed logical order — only
        // the within-group sequence changes. (consistency.rs depends on this.)
        for sort in [SortMode::Layer, SortMode::Downstream, SortMode::Tests] {
            let list = build_model_list(&sort_fixture(), sort);
            let layers: Vec<&str> = list.groups.iter().map(|g| g.layer.as_str()).collect();
            assert_eq!(layers, vec!["staging", "intermediate"], "sort {sort:?}");
        }
    }

    #[test]
    fn sort_layer_is_byte_identical_across_a_varied_fixture() {
        // The default build must be unchanged regardless of the new field data —
        // every non-test call site that wants today's behaviour passes Layer.
        let dag = sort_fixture();
        let a = build_model_list(&dag, SortMode::Layer);
        let b = build_model_list(&dag, SortMode::default());
        assert_eq!(a, b, "SortMode::default() == SortMode::Layer");
    }

    #[test]
    fn sort_mode_next_cycles_layer_downstream_tests() {
        assert_eq!(SortMode::Layer.next(), SortMode::Downstream);
        assert_eq!(SortMode::Downstream.next(), SortMode::Tests);
        assert_eq!(SortMode::Tests.next(), SortMode::Layer);
    }

    #[test]
    fn match_indices_marks_the_subsequence_positions() {
        // Empty query highlights nothing.
        assert_eq!(match_indices("stg_a", ""), Vec::<usize>::new());
        // "sa" matches stg_a at s(0) and a(4).
        assert_eq!(match_indices("stg_a", "sa"), vec![0, 4]);
        // The full prefix-ish subsequence "stga".
        assert_eq!(match_indices("stg_a", "stga"), vec![0, 1, 2, 4]);
        // Case-insensitive: an uppercase query matches the lowercase name.
        assert_eq!(match_indices("stg_a", "STGA"), vec![0, 1, 2, 4]);
        // A non-match (no subsequence) highlights nothing.
        assert_eq!(match_indices("stg_a", "zzz"), Vec::<usize>::new());
        // An incomplete subsequence (runs out of name) is NOT a match.
        assert_eq!(match_indices("stg_a", "stgaa"), Vec::<usize>::new());
        // Whitespace in the query is ignored (same rule as name_matches_query).
        assert_eq!(match_indices("stg_a", "s a"), vec![0, 4]);
    }

    #[test]
    fn match_indices_agrees_with_name_matches_query() {
        // The highlighter accepts exactly what the filter accepts: a non-empty
        // match returns a non-empty index set, a reject returns empty.
        for (name, query) in [("stg_a", "sa"), ("int_shoppers", "ip"), ("marts_x", "mx")] {
            assert!(name_matches_query(name, query));
            assert!(!match_indices(name, query).is_empty(), "{name} ~ {query}");
        }
        for (name, query) in [("stg_a", "zzz"), ("int_x", "qqq")] {
            assert!(!name_matches_query(name, query));
            assert!(match_indices(name, query).is_empty(), "{name} !~ {query}");
        }
    }
}
