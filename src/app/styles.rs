//! Per-node lineage render attributes: the materialization class (always) plus
//! the ACTIVE lens's tint, the root↔cursor focus dim, and the on-path band.

use std::collections::BTreeMap;
use std::collections::HashSet;

use crate::{
    coverage_gap, CellAttr, Layout, LensTint, LineageLens, MaterializationClass, NodeInfo,
};

use super::analysis::layer_violation_edges;
use super::App;

impl App {
    /// Build the per-node render attributes for a lineage [`Layout`], folding in
    /// the materialization class (→ colour) always, plus three render-only
    /// overlays that share the foreground/background channels:
    /// - `lens`: the ACTIVE lineage lens's [`LensTint`] for this node (computed by
    ///   [`lens_tint`](App::lens_tint)); `None` when the lens is `Off`.
    /// - `dimmed`: focus dim — set on every node NOT on the root↔cursor path
    ///   whenever the path is non-empty (i.e. the cursor is off-root). Lens-
    ///   independent; the render layer dims its foreground so the path nodes pop.
    /// - `on_path`: the node is on the root↔cursor path (the orthogonal bg band).
    ///
    /// Keyed by `unique_id` over the layout's nodes; fed to
    /// [`Layout::apply_node_styles`] by the event loop after `layout()`.
    ///
    /// Scope: path highlight + dim are NODE-BOXES-ONLY — `Layout` exposes no
    /// edge→cell map, and `apply_node_styles` stamps `rects` only, so the
    /// connector cells between path nodes stay Plain. (Tests show up as the
    /// `tests:N` bottom-border label, drawn by `layout()` itself from
    /// `NodeInfo::test_count` — no style needed here.)
    pub fn lineage_styles(&self, lay: &Layout) -> BTreeMap<String, CellAttr> {
        // CACHE-KEY INVARIANT: this function's output is memoized inside the
        // styled layout (`cache.rs::LayoutKey`). Every `App` input read here —
        // directly or transitively (lens, cursor/path, the Dag) — must be part
        // of that key, or a change to it will NOT invalidate the cached layout.
        let lens = self.ui_state.lens();
        let path = self.lineage_path_set(); // empty unless the cursor is off-root
        let dimming = !path.is_empty();
        // The LayerViolation lens needs the project-wide incident-node set; compute
        // it ONCE here (not per node) so it is O(edges) regardless of subgraph size.
        let violation_nodes: std::collections::HashSet<String> =
            if lens == LineageLens::LayerViolation {
                layer_violation_edges(&self.dag)
                    .into_iter()
                    .flat_map(|(p, c)| [p, c])
                    .collect()
            } else {
                std::collections::HashSet::new()
            };
        lay.rects
            .keys()
            .map(|uid| {
                let node = self.dag.get(uid);
                let rt = node.map(|n| n.resource_type.as_str()).unwrap_or("");
                let mat = self.dag.detail(uid).and_then(|d| d.materialized.as_deref());
                let attr = CellAttr {
                    class: MaterializationClass::classify(rt, mat),
                    lens: self.lens_tint(lens, uid, node, &violation_nodes),
                    // Off-path nodes dim only while a path exists (cursor off-root).
                    dimmed: dimming && !path.contains(uid),
                    on_path: path.contains(uid),
                };
                (uid.clone(), attr)
            })
            .collect()
    }

    /// The per-EDGE render attributes for a lineage [`Layout`]: the connector
    /// twin of [`lineage_styles`](App::lineage_styles), keyed like
    /// [`Layout::edge_cells`] (`(parent, child)`). Empty when the cursor is home
    /// (no path → connectors stay Plain, the frozen default render). When a
    /// root↔cursor path exists, its edges get the `on_path` background band and
    /// every OTHER edge dims — the same focus treatment the nodes get, extended
    /// to the lines between them.
    ///
    /// CACHE-KEY INVARIANT: memoized inside the styled layout like
    /// `lineage_styles` — inputs are the cursor/path and the subgraph edges,
    /// all already part of `cache.rs::LayoutKey`.
    pub fn lineage_edge_styles(&self, lay: &Layout) -> BTreeMap<(String, String), CellAttr> {
        let path: HashSet<(String, String)> = self.lineage_path_edges().into_iter().collect();
        if path.is_empty() {
            return BTreeMap::new();
        }
        lay.edge_cells
            .keys()
            .map(|key| {
                let on_path = path.contains(key);
                let attr = CellAttr {
                    on_path,
                    dimmed: !on_path,
                    ..Default::default()
                };
                (key.clone(), attr)
            })
            .collect()
    }

    /// Per-column layer annotations for the lineage pane's bottom-border band
    /// (the Layer lens's companion): for each layout column whose MODELS
    /// unanimously share one `first_dir` layer, a [`LayerBand`] with that
    /// layer's name and [`layer_tint`] — so the band can never disagree with
    /// the node tints. A column with mixed layers, or with no models at all
    /// (e.g. a pure-source column), gets no band rather than a lie.
    ///
    /// Deterministic: aggregated into a `BTreeMap` by column index; every node
    /// in a column shares its `rect.x`, and the width is the column's widest
    /// box (the same definition the layout used).
    pub fn layer_bands(&self, lay: &Layout) -> Vec<crate::ui::LayerBand> {
        use std::collections::BTreeMap;
        struct Col {
            x: usize,
            width: usize,
            label: Option<String>, // None = no model seen yet
            unanimous: bool,
        }
        let mut cols: BTreeMap<usize, Col> = BTreeMap::new();
        for (uid, rect) in &lay.rects {
            let Some(&c) = lay.columns.get(uid) else {
                continue;
            };
            let entry = cols.entry(c).or_insert(Col {
                x: rect.x,
                width: 0,
                label: None,
                unanimous: true,
            });
            entry.width = entry.width.max(rect.width);
            let Some(node) = self.dag.get(uid) else {
                continue;
            };
            if node.resource_type != "model" {
                continue; // sources/seeds/snapshots neither label nor disqualify
            }
            let layer = crate::model_list::first_dir(node)
                .unwrap_or("other")
                .to_string();
            match &entry.label {
                None => entry.label = Some(layer),
                Some(prev) if *prev != layer => entry.unanimous = false,
                Some(_) => {}
            }
        }
        cols.into_values()
            .filter_map(|col| {
                let label = col.label.filter(|_| col.unanimous)?;
                let tint = layer_tint_of(&label);
                Some(crate::ui::LayerBand {
                    x: col.x,
                    width: col.width,
                    label,
                    tint,
                })
            })
            .collect()
    }

    /// The [`LensTint`] for one node under the ACTIVE `lens`. The single place each
    /// lens's per-node metric is mapped to a semantic tint (the render layer turns
    /// the tint into a `Color`); `violation_nodes` is the precomputed project-wide
    /// incident set (empty unless the lens is `LayerViolation`). Returns
    /// `LensTint::None` for `Off` and for nodes a lens doesn't tint.
    fn lens_tint(
        &self,
        lens: LineageLens,
        uid: &str,
        node: Option<&NodeInfo>,
        violation_nodes: &std::collections::HashSet<String>,
    ) -> LensTint {
        match lens {
            LineageLens::Off => LensTint::None,
            // Coverage: exactly the old `untested` rule (a testable resource with
            // zero tests). Same `coverage_gap` base as the status cov% + stats.
            LineageLens::Coverage => {
                if node.is_some_and(coverage_gap) {
                    LensTint::Warn
                } else {
                    LensTint::None
                }
            }
            // Degree heat: transitive-downstream "blast radius". Buckets:
            //   n == 0     → None     (a leaf taints nothing downstream)
            //   1..=2      → HeatLow
            //   3..=6      → HeatMid
            //   >= 7       → HeatHigh
            LineageLens::DegreeHeat => match self.dag.downstream(uid).len() {
                0 => LensTint::None,
                1..=2 => LensTint::HeatLow,
                3..=6 => LensTint::HeatMid,
                _ => LensTint::HeatHigh,
            },
            // Layer: tint a MODEL by its dbt logical layer (the SAME `first_dir`
            // classification the list groups by). Sources/seeds/snapshots get None
            // so their distinctive materialization-class colour keeps showing.
            LineageLens::Layer => match node {
                Some(n) if n.resource_type == "model" => layer_tint(n),
                _ => LensTint::None,
            },
            // Layer violations: tint any node incident to a backward edge.
            LineageLens::LayerViolation => {
                if violation_nodes.contains(uid) {
                    LensTint::Violation
                } else {
                    LensTint::None
                }
            }
        }
    }
}

/// A model's layer tint from its `first_dir` layer — shared by the Layer lens
/// (per node) and the layer bands (per column), so the two can never disagree.
fn layer_tint(n: &NodeInfo) -> LensTint {
    layer_tint_of(crate::model_list::first_dir(n).unwrap_or(""))
}

/// The ONE layer-name → `LensTint::Layer*` mapping.
fn layer_tint_of(layer: &str) -> LensTint {
    match layer {
        "staging" => LensTint::LayerStaging,
        "intermediate" => LensTint::LayerIntermediate,
        "marts" => LensTint::LayerMarts,
        "utilities" => LensTint::LayerUtilities,
        _ => LensTint::LayerOther,
    }
}
