//! Sibling-collapse for the lineage pane: when one column stacks more than
//! `threshold` children of the same parent, the tail contracts into a single
//! `+M more` marker node the layout treats like any other box.
//!
//! This is the pure, ratatui-free transform — the display stage between
//! `Dag::subgraph_view` (the rooted subgraph) and `layout` (the grid). It
//! consumes and produces plain [`Subgraph`]s, so `layout` needs no marker
//! awareness at all: a marker is just a [`NodeInfo`] with the synthetic
//! [`COLLAPSE_RESOURCE_TYPE`], and the hidden members' edges are rewired
//! through it like any contraction.
//!
//! Determinism (the repo's hard rule) holds by construction: members are
//! collected in the subgraph's `unique_id`-sorted order, grouping iterates a
//! `BTreeMap`, the output node/edge lists are re-sorted, and the keep-set is
//! only ever read via `contains` — so two calls on the same input produce
//! bit-identical output.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::layout::longest_path_columns;
use crate::{Edge, NodeInfo, Subgraph};

/// The default sibling-group size over which a column collapses the tail into
/// a `+M more` marker (`config.yml`'s `lineage_collapse`; `0` disables the
/// markers entirely).
pub const DEFAULT_COLLAPSE_THRESHOLD: usize = 8;

/// The marker's `resource_type` — a value no dbt manifest can emit, so
/// `Dag::get` never resolves a marker uid and every resource-type dispatch
/// (`Enter` re-root, `s` preview, coverage, lenses) treats it as "not a real
/// node" before it is even looked up.
pub const COLLAPSE_RESOURCE_TYPE: &str = "more";

/// The suffix that turns the run's lead `unique_id` into the marker's.
/// `+` never appears in a real dbt `unique_id` and sorts before `.`/letters,
/// so the marker lands exactly where the collapsed run begins (right after
/// the last kept sibling smaller than it) and can never collide with a real
/// node.
const MARKER_SUFFIX: &str = "+more";

/// One collapsed sibling group, recorded under its marker's `unique_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapseGroup {
    /// The expansion key: the hub parent's `unique_id`. A group is the
    /// children sharing one deepest parent, so the parent alone identifies it
    /// — stable across keep-set changes that would reshuffle the marker uid.
    pub key: String,
    /// The member `unique_id`s the marker stands in for, sorted. Hidden while
    /// the group is collapsed, shown (behind a `-M less` marker) once expanded.
    pub members: Vec<String>,
}

/// The collapse transform's output: the displayable subgraph plus the marker
/// index the interaction layer needs (hit-test, the `Enter` toggle, and the
/// lineage-search auto-reveal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collapsed {
    /// What `layout` draws: `nodes`/`edges`/`selected` with markers in and the
    /// hidden members' edges rewired through their marker.
    pub subgraph: Subgraph,
    /// `marker uid -> the group it stands for` — every marker node present in
    /// [`subgraph`](Collapsed::subgraph) (collapsed `+M more` AND expanded
    /// `-M less` markers alike).
    pub groups: BTreeMap<String, CollapseGroup>,
}

/// The label a marker box draws: `+M more` while its group hides members,
/// `-M less` once expanded (the marker stays as the collapse toggle).
fn marker_label(hidden: usize, expanded: bool) -> String {
    if expanded {
        format!("-{hidden} less")
    } else {
        format!("+{hidden} more")
    }
}

/// Apply the sibling-collapse to `sg` — the display-stage seam between
/// `lineage_display_subgraph` and `layout`.
///
/// `expanded` holds the group keys ([`CollapseGroup::key`]) the user opened;
/// `priority` holds `unique_id`s that must stay visible no matter where they
/// sort (the cursor, the root↔cursor path nodes, bookmarks — the caller's
/// keep-alive set). Kept members: `threshold` slots filled priority-first,
/// then in `unique_id` order — every priority member is shown even when it
/// alone would overflow the threshold.
///
/// Contract: `sg.selected` must name a node that survives (the caller lists
/// it in `priority`); membership is NOT re-validated here.
pub fn collapse_siblings(
    sg: &Subgraph,
    threshold: usize,
    expanded: &BTreeSet<String>,
    priority: &HashSet<String>,
) -> Collapsed {
    let no_op = |groups| Collapsed {
        subgraph: sg.clone(),
        groups,
    };
    // `0` disables the feature outright; a subgraph that can't even fill the
    // threshold can never overflow it either.
    if threshold == 0 || sg.nodes.len() <= threshold {
        return no_op(BTreeMap::new());
    }

    let cols = longest_path_columns(sg);
    // In-subgraph parents per child (sorted + deduped for determinism).
    let mut parents_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &sg.edges {
        parents_of
            .entry(e.child.as_str())
            .or_default()
            .push(e.parent.as_str());
    }
    for ps in parents_of.values_mut() {
        ps.sort_unstable();
        ps.dedup();
    }

    // A node's sibling group is the children sharing its DEEPEST parent (the
    // parent that sets its longest-path column). Ties between equally-deep
    // parents go to the smallest `unique_id`, so every node lands in at most
    // one candidate group and grouping is fully deterministic.
    let mut members_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in &sg.nodes {
        let Some(ps) = parents_of.get(node.unique_id.as_str()) else {
            continue; // a column-0 root has no siblings to collapse with
        };
        let deepest = ps
            .iter()
            .min_by_key(|p| (std::cmp::Reverse(cols.get(**p).copied().unwrap_or(0)), **p))
            .copied()
            .expect("non-empty parents");
        members_of
            .entry(deepest)
            .or_default()
            .push(node.unique_id.as_str());
    }

    let mut groups: BTreeMap<String, CollapseGroup> = BTreeMap::new();
    // member uid -> its marker uid, for every member a COLLAPSED group hides.
    let mut hidden_marker: HashMap<&str, String> = HashMap::new();
    // Every marker node to draw (collapsed `+M more` AND expanded `-M less`).
    let mut markers: Vec<NodeInfo> = Vec::new();
    // The `-M less` markers' leaf edge back to the grouping parent (expanded
    // groups keep their members' real edges; only the marker needs wiring).
    let mut expanded_edges: Vec<Edge> = Vec::new();

    for (parent, members) in &members_of {
        if members.len() <= threshold {
            continue;
        }
        // Keep-set: priority members always show (cursor/path/bookmarks), the
        // remaining slots fill in `unique_id` order. `members` is already
        // uid-sorted, so both partitions keep that order.
        let (pri, rest): (Vec<&str>, Vec<&str>) =
            members.iter().partition(|uid| priority.contains(**uid));
        let mut shown: HashSet<&str> = pri.iter().copied().collect();
        shown.extend(pri.iter().chain(rest.iter()).take(threshold).copied());
        let standins: Vec<&str> = members
            .iter()
            .copied()
            .filter(|uid| !shown.contains(uid))
            .collect();
        if standins.is_empty() {
            continue; // every member is priority-kept — nothing to hide
        }
        // The marker's uid derives from `members[threshold]` — the member that
        // leads the hidden run under the DEFAULT keep order — NOT from the
        // priority-dependent `standins[0]`. That keeps the uid stable while a
        // priority member reshuffles the tail (cursor on a revealed sibling):
        // a `+M more`/`-M less` cursor stays a live uid across keep-set
        // changes instead of vanishing out from under the caret.
        let marker_uid = format!("{}{}", members[threshold], MARKER_SUFFIX);
        let is_expanded = expanded.contains(*parent);
        groups.insert(
            marker_uid.clone(),
            CollapseGroup {
                key: (*parent).to_string(),
                members: standins.iter().map(|s| (*s).to_string()).collect(),
            },
        );
        markers.push(NodeInfo {
            unique_id: marker_uid.clone(),
            name: marker_label(standins.len(), is_expanded),
            resource_type: COLLAPSE_RESOURCE_TYPE.to_string(),
            ..Default::default()
        });
        if is_expanded {
            expanded_edges.push(Edge {
                parent: (*parent).to_string(),
                child: marker_uid,
            });
        } else {
            for uid in &standins {
                hidden_marker.insert(uid, marker_uid.clone());
            }
        }
    }

    if groups.is_empty() {
        return no_op(groups);
    }

    // Edges: route through markers wherever a hidden endpoint stood — the
    // contraction preserves connectivity (hidden->hidden inside one group
    // collapses away; across groups it becomes a marker->marker edge).
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    for e in &sg.edges {
        let p = hidden_marker.get(e.parent.as_str()).map(String::as_str);
        let c = hidden_marker.get(e.child.as_str()).map(String::as_str);
        match (p, c) {
            (Some(mp), Some(mc)) if mp == mc => {} // absorbed into the marker
            (Some(mp), Some(mc)) => {
                edges.insert((mp.to_string(), mc.to_string()));
            }
            (Some(mp), None) => {
                edges.insert((mp.to_string(), e.child.clone()));
            }
            (None, Some(mc)) => {
                edges.insert((e.parent.clone(), mc.to_string()));
            }
            (None, None) => {
                edges.insert((e.parent.clone(), e.child.clone()));
            }
        }
    }
    for e in expanded_edges {
        edges.insert((e.parent, e.child));
    }

    // Nodes: the survivors plus every marker, re-sorted — the `+` suffix lands
    // each marker at the head of the run it replaced.
    let hidden: HashSet<&str> = hidden_marker.keys().copied().collect();
    let mut nodes: Vec<NodeInfo> = sg
        .nodes
        .iter()
        .filter(|n| !hidden.contains(n.unique_id.as_str()))
        .cloned()
        .collect();
    nodes.extend(markers);
    nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));

    Collapsed {
        subgraph: Subgraph {
            selected: sg.selected.clone(),
            nodes,
            edges: edges
                .into_iter()
                .map(|(parent, child)| Edge { parent, child })
                .collect(),
        },
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Subgraph` literal: `hub` -> `kids` children in one column.
    fn hub_sg(kids: usize) -> Subgraph {
        let mut nodes = vec![NodeInfo {
            unique_id: "model.p.hub".to_string(),
            name: "hub".to_string(),
            resource_type: "model".to_string(),
            ..Default::default()
        }];
        let mut edges = Vec::new();
        for i in 0..kids {
            let uid = format!("model.p.c{i:02}");
            nodes.push(NodeInfo {
                unique_id: uid.clone(),
                name: format!("c{i:02}"),
                resource_type: "model".to_string(),
                ..Default::default()
            });
            edges.push(Edge {
                parent: "model.p.hub".to_string(),
                child: uid,
            });
        }
        nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));
        Subgraph {
            selected: "model.p.hub".to_string(),
            nodes,
            edges,
        }
    }

    /// The only marker in a collapsed view, or a test failure naming why.
    fn only_marker(c: &Collapsed) -> &NodeInfo {
        assert_eq!(c.groups.len(), 1, "expected exactly one marker group");
        let marker = c.groups.keys().next().unwrap();
        c.subgraph
            .nodes
            .iter()
            .find(|n| &n.unique_id == marker)
            .expect("the marker is a node")
    }

    #[test]
    fn threshold_boundary_is_exact() {
        // `len > threshold` collapses; `len == threshold` does not.
        let sg = hub_sg(8);
        let c = collapse_siblings(&sg, 8, &BTreeSet::new(), &HashSet::new());
        assert!(
            c.groups.is_empty(),
            "8 siblings at threshold 8: no collapse"
        );
        let sg = hub_sg(9);
        let c = collapse_siblings(&sg, 8, &BTreeSet::new(), &HashSet::new());
        assert_eq!(c.groups.len(), 1, "9 siblings at threshold 8: collapse");
        let m = only_marker(&c);
        assert_eq!(m.name, "+1 more");
        assert_eq!(m.unique_id, "model.p.c08+more");
    }

    #[test]
    fn collapse_keeps_first_n_by_unique_id_and_rewires_edges() {
        let sg = hub_sg(20);
        let c = collapse_siblings(&sg, 8, &BTreeSet::new(), &HashSet::new());
        let m = only_marker(&c);
        assert_eq!(m.name, "+12 more");
        // Kept: c00..c07 (unique_id order); hidden c08..c19 behind the marker.
        let ids: BTreeSet<&str> = c
            .subgraph
            .nodes
            .iter()
            .map(|n| n.unique_id.as_str())
            .collect();
        for i in 0..8 {
            assert!(ids.contains(format!("model.p.c{i:02}").as_str()));
        }
        for i in 8..20 {
            assert!(!ids.contains(format!("model.p.c{i:02}").as_str()));
        }
        assert!(ids.contains("model.p.c08+more"), "marker is a node");
        // Edges: hub -> each kept child + hub -> marker.
        let edges: BTreeSet<(&str, &str)> = c
            .subgraph
            .edges
            .iter()
            .map(|e| (e.parent.as_str(), e.child.as_str()))
            .collect();
        assert!(edges.contains(&("model.p.hub", "model.p.c08+more")));
        assert_eq!(edges.len(), 9, "8 kept children + the marker: {edges:?}");
    }

    #[test]
    fn priority_members_survive_past_the_cutoff() {
        let sg = hub_sg(12);
        // A cursor deep in the tail must stay visible (keep-priority).
        let priority: HashSet<String> = ["model.p.c10".to_string()].into_iter().collect();
        let c = collapse_siblings(&sg, 4, &BTreeSet::new(), &priority);
        let ids: BTreeSet<&str> = c
            .subgraph
            .nodes
            .iter()
            .map(|n| n.unique_id.as_str())
            .collect();
        assert!(ids.contains("model.p.c10"), "priority member kept");
        let m = only_marker(&c);
        // kept = c10 + first 4 of [c10, c00..] = c10,c00,c01,c02 → 8 hidden.
        assert_eq!(m.name, "+8 more");
    }

    #[test]
    fn expanded_group_shows_members_and_a_less_marker() {
        let sg = hub_sg(12);
        let expanded: BTreeSet<String> = ["model.p.hub".to_string()].into_iter().collect();
        let c = collapse_siblings(&sg, 4, &expanded, &HashSet::new());
        let ids: BTreeSet<&str> = c
            .subgraph
            .nodes
            .iter()
            .map(|n| n.unique_id.as_str())
            .collect();
        for i in 0..12 {
            assert!(ids.contains(format!("model.p.c{i:02}").as_str()));
        }
        let m = only_marker(&c);
        assert_eq!(m.name, "-8 less", "expanded marker offers re-collapse");
        // The expanded marker is a leaf off the hub.
        let edges: BTreeSet<(&str, &str)> = c
            .subgraph
            .edges
            .iter()
            .map(|e| (e.parent.as_str(), e.child.as_str()))
            .collect();
        assert!(edges.contains(&("model.p.hub", "model.p.c04+more")));
        // The marker's would-be stand-ins (same partition as collapsed).
        assert_eq!(
            c.groups["model.p.c04+more"].members,
            (4..12)
                .map(|i| format!("model.p.c{i:02}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn hidden_children_edges_rewire_through_the_marker() {
        // hub -> k0..k9 (collapse), k5 -> leaf (visible child of a hidden
        // member): the leaf's incoming edge must become marker -> leaf.
        let mut sg = hub_sg(10);
        sg.nodes.push(NodeInfo {
            unique_id: "model.p.leaf".to_string(),
            name: "leaf".to_string(),
            resource_type: "model".to_string(),
            ..Default::default()
        });
        sg.edges.push(Edge {
            parent: "model.p.c05".to_string(),
            child: "model.p.leaf".to_string(),
        });
        sg.nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));
        let c = collapse_siblings(&sg, 4, &BTreeSet::new(), &HashSet::new());
        let edges: BTreeSet<(&str, &str)> = c
            .subgraph
            .edges
            .iter()
            .map(|e| (e.parent.as_str(), e.child.as_str()))
            .collect();
        assert!(edges.contains(&("model.p.c04+more", "model.p.leaf")));
        assert!(!edges.contains(&("model.p.c05", "model.p.leaf")));
    }

    #[test]
    fn threshold_zero_disables() {
        let sg = hub_sg(20);
        let c = collapse_siblings(&sg, 0, &BTreeSet::new(), &HashSet::new());
        assert!(c.groups.is_empty());
        assert_eq!(c.subgraph.nodes.len(), 21, "every sibling stays");
    }

    #[test]
    fn marker_uid_is_stable_across_keep_sets() {
        // The marker uid derives from `members[threshold]` — the lead of the
        // default hidden run — so a priority member reshuffling the tail can
        // never rename the marker out from under a parked cursor.
        let sg = hub_sg(12);
        let m0 = only_marker(&collapse_siblings(
            &sg,
            4,
            &BTreeSet::new(),
            &HashSet::new(),
        ))
        .unique_id
        .clone();
        // A cursor/priority node deep in the tail (c10 is kept by priority):
        // the hidden run now starts at c03, but the marker uid stays c04+more.
        let priority: HashSet<String> = ["model.p.c10".to_string()].into_iter().collect();
        let c = collapse_siblings(&sg, 4, &BTreeSet::new(), &priority);
        let m1 = only_marker(&c);
        assert_eq!(
            m1.unique_id, m0,
            "keep-set changes must not rename the marker"
        );
        // A cursor parked ON the marker contributes no member priority, so the
        // recomputed view still names the same marker — the uid stays live.
        let on_marker: HashSet<String> = [m0.clone()].into_iter().collect();
        let c2 = collapse_siblings(&sg, 4, &BTreeSet::new(), &on_marker);
        assert_eq!(
            only_marker(&c2).unique_id,
            m0,
            "cursor on marker keeps it alive"
        );
    }

    #[test]
    fn layout_sees_the_marker_in_the_sibling_column() {
        // The structured-map contract: the marker lands in column 1 with a
        // real rect, and geometry is glyph-mode identical.
        let sg = hub_sg(20);
        for mode in [crate::GlyphMode::Unicode, crate::GlyphMode::Ascii] {
            let c = collapse_siblings(&sg, 8, &BTreeSet::new(), &HashSet::new());
            let lay = crate::layout_mode(&c.subgraph, mode);
            let marker = lay
                .rects
                .keys()
                .find(|u| u.ends_with("+more"))
                .cloned()
                .expect("marker rect exists");
            assert_eq!(
                lay.columns[&marker], 1,
                "marker sits in the column it collapsed"
            );
            assert_eq!(lay.rects.len(), 1 + 8 + 1, "hub + 8 kept + marker");
        }
    }
}
