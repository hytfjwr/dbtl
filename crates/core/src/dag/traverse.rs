use super::*;

/// Prune an adjacency map (`parent_map` / `child_map`) to the kept node set.
///
/// Drops any key not in `kept`, and filters each neighbour list to kept nodes.
/// This is the "prune-first" step that makes traversal correct: an excluded
/// node can never be an endpoint or a transit hop (e.g. `model -> test -> model`
/// is broken, never silently bridged).
pub(super) fn prune_adjacency(
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
pub(super) fn closure(adjacency: &HashMap<String, Vec<String>>, start: &str) -> HashSet<String> {
    closure_depth(adjacency, start, None)
}

/// Level-by-level BFS closure limited to `max_depth` hops (`None` = unlimited),
/// excluding the seed. Cycle-safe (the `visited` set dedups). The depth cap
/// powers the lineage depth-limit view.
pub(super) fn closure_depth(
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
