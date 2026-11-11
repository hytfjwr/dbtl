use super::*;

impl Dag {
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
