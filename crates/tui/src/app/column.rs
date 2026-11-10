//! Column-level lineage focus (Issue 09): the traced-column state plus the
//! derived payload every surface consumes — the filtered subgraph, the
//! per-box column rows, the status chip, the ColumnImpact lens set, and the
//! `Mode::Columns` modal rows.
//!
//! The lazy [`ColumnLineage`] engine borrows the App's `Dag` + resolver, so
//! every answer is MATERIALIZED into plain data on enter/step — nothing holds
//! a borrow, and `SubgraphKey`/`LayoutKey` carry the column's identity
//! (`(node uid, column)`) so a step or exit invalidates by key comparison.

use std::collections::{BTreeMap, BTreeSet};

use crate::action::{ColumnRow, ColumnView, Direction};
use crate::{
    ColLine, ColumnEdge, ColumnEdgeKind, ColumnLineage, ColumnRef, Confidence, Edge, Focus,
    NameMatchResolver, NodeInfo, Subgraph,
};

use super::App;

/// The traced-column state (Issue 09 A): which column is being traced plus
/// the precomputed payload the filtered subgraph, the box column rows, the
/// status chip, and the lens all read. Rebuilt on every enter/step — the
/// lazy engine memoizes per call, so a step pays only for the region it
/// touches.
pub(crate) struct ColumnFocus {
    /// The traced column (`node` uid + normalized `column`).
    pub col: ColumnRef,
    /// Every node the column's up/down closure touches (incl. `col.node`) —
    /// the filtered subgraph's node set. `BTreeSet` = uid-sorted iteration.
    pub related: BTreeSet<String>,
    /// The nodes the traced column reaches DOWNSTREAM (incl. its own) — the
    /// [`crate::LensTint::ColumnImpact`] membership test.
    pub down_nodes: BTreeSet<String>,
    /// `unique_id -> related column rows` inside that node's box — the
    /// [`layout_columns`](crate::layout_columns) input. Each list is already
    /// in display order (name-sorted: `on_trace` iterates `(node, column)`
    /// order). The rows carry the edge KIND, not a baked mark glyph — layout
    /// resolves the mark against the draw's glyph mode.
    pub lines: BTreeMap<String, Vec<ColLine>>,
    /// The status chip's `(upstream, downstream)` counts: ONE-HOP column-edge
    /// neighbours of the traced column — the same "direct parents in / direct
    /// children out" rule as the structure modal's per-column `↑u ↓d`
    /// ([`column_counts`](crate::ColumnLineage::column_counts)), NOT the
    /// transitive closure the modal's section headers quote.
    pub counts: (usize, usize),
}

/// How "interesting" an edge kind is for the 1-cell row mark — a computed
/// Transform outranks a Rename, which outranks a row-shaping FilterOrJoin,
/// which outranks a carried-through Passthrough. (The enum's own `Ord` puts
/// FilterOrJoin last — fine for wire order, wrong for display weight.)
fn kind_rank(kind: ColumnEdgeKind) -> u8 {
    match kind {
        ColumnEdgeKind::Passthrough => 0,
        ColumnEdgeKind::FilterOrJoin => 1,
        ColumnEdgeKind::Rename => 2,
        ColumnEdgeKind::Transform => 3,
    }
}

/// The deterministic step candidate for [`App::column_focus_step`]: a column
/// with the SAME normalized name as the traced one first (the common
/// passthrough continuation), else the smallest by `ColumnRef`'s
/// `(node, column)` order. `None` = no hop in that direction.
fn pick_column_step(col: &ColumnRef, mut cands: Vec<ColumnRef>) -> Option<ColumnRef> {
    cands.sort();
    cands.dedup();
    cands
        .iter()
        .find(|c| c.column == col.column)
        .cloned()
        .or_else(|| cands.first().cloned())
}

impl App {
    /// The effective column focus: `None` while unfocused, AND while the
    /// whole-graph overview is up (the focus state survives an overview
    /// round-trip — only the *display* suppresses it). Every reader of the
    /// focus (`subgraph_rc`, the styled layout, the lens, the status chip)
    /// goes through this so "effective" can't drift between surfaces.
    pub(super) fn active_column_focus(&self) -> Option<&ColumnFocus> {
        if self.ui_state.overview() {
            None
        } else {
            self.column_focus.as_ref()
        }
    }

    /// The cache-key identity of the effective focus (`(node uid, column)`),
    /// `None` exactly when [`active_column_focus`](App::active_column_focus)
    /// is — so the memoized subgraph/layout keys match whatever the pipeline
    /// will actually compute.
    pub(super) fn column_focus_key(&self) -> Option<(String, String)> {
        self.active_column_focus()
            .map(|f| (f.col.node.clone(), f.col.column.clone()))
    }

    /// Compute the [`ColumnFocus`] payload for `col`: its up/down column
    /// closures, the related-node set, the traced edge set, and the per-box
    /// [`ColLine`] rows. Pure over `&self` — the lazy engine resolves only
    /// the touched region (its memos are per call; the resolver's own memos
    /// make repeat enters cheap).
    fn compute_column_focus(&self, col: &ColumnRef) -> ColumnFocus {
        let engine = ColumnLineage::new(&self.dag, &self.cll);
        let up = engine.closure_up(col);
        let down = engine.closure_down(col);
        // The trace membership: both closures plus the traced column itself.
        let mut on_trace: BTreeSet<ColumnRef> = BTreeSet::new();
        on_trace.extend(up.iter().cloned());
        on_trace.extend(down.iter().cloned());
        on_trace.insert(col.clone());

        // The traced edges: every column edge with BOTH endpoints on the
        // trace, resolved per touched node (lazy — each node resolves once
        // against the resolver's memo) and `BTreeSet`-deduped → sorted,
        // deterministic.
        let related: BTreeSet<String> = on_trace.iter().map(|c| c.node.clone()).collect();
        let mut edges: BTreeSet<ColumnEdge> = BTreeSet::new();
        for uid in &related {
            for e in engine.in_edges(uid).iter() {
                if on_trace.contains(&e.from) && on_trace.contains(&e.to) {
                    edges.insert(e.clone());
                }
            }
        }
        let edges: Vec<ColumnEdge> = edges.into_iter().collect();

        // Per-node box rows: the node's on-trace columns, each marked by the
        // heaviest incident edge kind, dimmed when inference is the only
        // basis it ever had, band-highlighted when it IS the traced column.
        // `on_trace` iterates `(node, column)` order, so each row list comes
        // out name-sorted for free.
        let mut lines: BTreeMap<String, Vec<ColLine>> = BTreeMap::new();
        for uid in &related {
            let rows: Vec<ColLine> = on_trace
                .iter()
                .filter(|c| &c.node == uid)
                .map(|cr| {
                    let inc: Vec<&ColumnEdge> = edges
                        .iter()
                        .filter(|e| e.from == *cr || e.to == *cr)
                        .collect();
                    let kind = inc
                        .iter()
                        .map(|e| e.kind)
                        .max_by_key(|k| kind_rank(*k))
                        .unwrap_or(ColumnEdgeKind::Passthrough);
                    let target = *cr == *col;
                    ColLine {
                        name: cr.column.clone(),
                        // The row carries the KIND — layout resolves the mark
                        // glyph (`>` for the traced column overrides it).
                        kind,
                        faint: !inc.is_empty()
                            && inc.iter().all(|e| e.confidence != Confidence::Exact),
                        target,
                    }
                })
                .collect();
            lines.insert(uid.clone(), rows);
        }

        let down_nodes: BTreeSet<String> = down
            .iter()
            .map(|c| c.node.clone())
            .chain(std::iter::once(col.node.clone()))
            .collect();
        ColumnFocus {
            col: col.clone(),
            related,
            down_nodes,
            lines,
            // ONE-HOP counts — the `↑u ↓d` surfaces (structure modal, README)
            // all speak direct-neighbour counts, so the status chip does too.
            counts: (engine.upstream(col).len(), engine.downstream(col).len()),
        }
    }

    /// Enter column focus on `col` (the structure modal's Enter/`c` on a
    /// column row, and the step target of [`column_focus_step`]): compute the
    /// trace payload, home the lineage cursor to the traced model, pin focus
    /// to the lineage pane — the trace IS the lineage view now — and toast
    /// the navigation hint. The caller owns the mode transition.
    ///
    /// [`column_focus_step`]: App::column_focus_step
    pub(super) fn column_focus_enter(&mut self, col: ColumnRef) {
        let name = self.node_name_or_uid(&col.node);
        self.column_focus = Some(self.compute_column_focus(&col));
        self.lineage_cursor = None;
        self.ui_state.set_focus(Focus::RightPane);
        // ASCII-only hint text — the toast renders in both glyph modes.
        self.set_notice(format!(
            "column: {name}.{} (h/l step, K modal, Esc exits)",
            col.column
        ));
    }

    /// Step the traced column one hop along its lineage (h = upstream, l =
    /// downstream): re-trace from the next column and home the cursor to its
    /// model. Deterministic via [`pick_column_step`]. No-op unfocused or at
    /// a trace end (the h/l arm already gated to the lineage pane).
    pub(super) fn column_focus_step(&mut self, dir: Direction) {
        let Some(focus) = &self.column_focus else {
            return;
        };
        let engine = ColumnLineage::new(&self.dag, &self.cll);
        let cands: Vec<ColumnRef> = match dir {
            Direction::Left | Direction::Up => engine
                .upstream(&focus.col)
                .iter()
                .map(|e| e.from.clone())
                .collect(),
            Direction::Down | Direction::Right => engine
                .downstream(&focus.col)
                .iter()
                .map(|e| e.to.clone())
                .collect(),
        };
        if let Some(next) = pick_column_step(&focus.col, cands) {
            self.column_focus_enter(next);
        }
    }

    /// The filtered model subgraph the column focus draws: `related` nodes
    /// (uid-sorted, matching `subgraph_view`'s order) plus every model edge
    /// whose endpoints are BOTH related — column edges only ever ride model
    /// edges, so the induced graph is connected through the trace.
    /// `selected` is the traced column's model: the emphasis/anchor home.
    pub(super) fn column_subgraph(&self, focus: &ColumnFocus) -> Subgraph {
        let nodes: Vec<NodeInfo> = focus
            .related
            .iter()
            .filter_map(|uid| self.dag.get(uid).cloned())
            .collect();
        let mut edges: Vec<Edge> = Vec::new();
        for uid in &focus.related {
            for parent in self.dag.direct_parents(uid) {
                if focus.related.contains(parent) {
                    edges.push(Edge {
                        parent: parent.clone(),
                        child: uid.clone(),
                    });
                }
            }
        }
        edges.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.child.cmp(&b.child)));
        edges.dedup();
        Subgraph {
            selected: focus.col.node.clone(),
            nodes,
            edges,
        }
    }

    /// The status-bar segment while a column is traced:
    /// `column:fct_orders.total_amount ↑3 ↓7` — the traced identity plus the
    /// ONE-HOP up/down column-edge counts (matching the structure modal's
    /// per-column `↑u ↓d`), in the Chrome badge glyphs (`^`/`v` in ASCII
    /// mode, like the list's dependency badges).
    pub fn column_status(&self) -> Option<String> {
        let f = self.active_column_focus()?;
        let name = self.node_name_or_uid(&f.col.node);
        let chrome = crate::ui::chrome(self.glyph_mode);
        Some(format!(
            "column:{name}.{} {}{} {}{}",
            f.col.column, chrome.badge_up, f.counts.0, chrome.badge_down, f.counts.1
        ))
    }

    /// The lineage-pane title suffix while a column is traced
    /// (` [col:name]`, pure ASCII — safe in both glyph modes). Composed into
    /// the title's suffix slot by the render layer.
    pub fn column_focus_label(&self) -> Option<String> {
        self.active_column_focus()
            .map(|f| format!(" [col:{}]", f.col.column))
    }

    /// The `Mode::Columns` payload for `col` (Issue 09 C): the upstream and
    /// downstream column closures as a hop-indented tree, each entry row
    /// carrying its node's uid for the Enter jump. Deterministic: each BFS
    /// level expands in `(node, column)` order and a shared `seen` set lists
    /// every column once, at its earliest hop.
    pub(super) fn columns_view_for(&self, col: &ColumnRef) -> ColumnView {
        let engine = ColumnLineage::new(&self.dag, &self.cll);
        // Shared across both directions: v0's FilterOrJoin over-attachment
        // can reach a column by two routes — it lists once, at the earliest.
        let mut seen: BTreeSet<ColumnRef> = BTreeSet::from([col.clone()]);
        let up_rows = self.column_modal_side(&engine, col, &mut seen, false);
        let down_rows = self.column_modal_side(&engine, col, &mut seen, true);

        let mut rows: Vec<ColumnRow> = Vec::new();
        rows.push(ColumnRow::header(format!("upstream ({}):", up_rows.len())));
        rows.extend(up_rows);
        rows.push(ColumnRow::header("this column:".to_string()));
        let cursor = rows.len();
        rows.push(ColumnRow {
            label: format!("  {}.{}", self.node_name_or_uid(&col.node), col.column),
            uid: Some(col.node.clone()),
            traced: true,
            faint: false,
        });
        rows.push(ColumnRow::header(format!(
            "downstream ({}):",
            down_rows.len()
        )));
        rows.extend(down_rows);
        ColumnView {
            title: format!("{}.{}", self.node_name_or_uid(&col.node), col.column),
            rows,
            cursor,
            scroll: 0,
        }
    }

    /// One direction of the column modal's tree: BFS from `col` (`down` picks
    /// `downstream`/`e.to`, else `upstream`/`e.from`), each level one more
    /// hop of indent, entries sorted by `(node, column)` within a level.
    /// `seen` is the shared dedupe across both directions.
    fn column_modal_side(
        &self,
        engine: &ColumnLineage<'_, NameMatchResolver>,
        col: &ColumnRef,
        seen: &mut BTreeSet<ColumnRef>,
        down: bool,
    ) -> Vec<ColumnRow> {
        let mut out = Vec::new();
        let mut frontier = vec![col.clone()];
        let mut depth = 0usize;
        loop {
            depth += 1;
            let mut next: Vec<(ColumnRef, ColumnEdgeKind, Confidence)> = Vec::new();
            for cur in &frontier {
                let hop = if down {
                    engine.downstream(cur)
                } else {
                    engine.upstream(cur)
                };
                for e in hop {
                    let tgt = if down { e.to } else { e.from };
                    if seen.insert(tgt.clone()) {
                        next.push((tgt, e.kind, e.confidence));
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            next.sort_by(|a, b| a.0.cmp(&b.0));
            let indent = "  ".repeat(depth);
            for (cr, kind, conf) in &next {
                out.push(ColumnRow {
                    label: format!(
                        "{indent}{}.{}  ({kind})",
                        self.node_name_or_uid(&cr.node),
                        cr.column
                    ),
                    uid: Some(cr.node.clone()),
                    traced: false,
                    faint: *conf != Confidence::Exact,
                });
            }
            frontier = next.into_iter().map(|(cr, _, _)| cr).collect();
            // `seen` already guarantees termination, but cap the depth so a
            // resolver bug can never hang the UI on a pathological graph.
            if depth >= 128 {
                break;
            }
        }
        out
    }
}
