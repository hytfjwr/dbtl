//! Per-node lineage render attributes: the materialization class (always) plus
//! the ACTIVE lens's tint, the root↔cursor focus dim, and the on-path band.

use std::collections::BTreeMap;

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
                Some(n) if n.resource_type == "model" => {
                    match crate::model_list::first_dir(n).unwrap_or("") {
                        "staging" => LensTint::LayerStaging,
                        "intermediate" => LensTint::LayerIntermediate,
                        "marts" => LensTint::LayerMarts,
                        "utilities" => LensTint::LayerUtilities,
                        _ => LensTint::LayerOther,
                    }
                }
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
