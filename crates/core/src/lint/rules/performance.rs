//! `performance` — graph-shape rules that predict wasted work at query time.

use std::collections::{HashMap, HashSet};

use super::*;

/// `chained_views`: a chain of consecutive view/ephemeral models at least
/// `threshold` (default 5) long. Node N's chain length is
/// `1 + max(chain length over its view/ephemeral model parents)` — `1` when
/// it has none, `0` when N itself isn't view/ephemeral. A finding lands only
/// on a chain END: a node at/above the threshold with no view/ephemeral
/// model child — and no package-severed child at all (the
/// [`PrunedAdjacent`](crate::PrunedAdjacent) record: a pruned consumer's
/// materialization is invisible, so the END can't be established).
/// `related` is the full longest chain upstream→downstream (the
/// flagged node last — it IS one of the chained views); several equal parents
/// resolve to the smallest `unique_id`. The memoized DFS walks the whole
/// graph once (O(nodes + edges)).
pub(super) fn chained_views(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let threshold = config
        .int_param(LintRule::ChainedViews, "threshold")
        .expect("the registry declares chained_views.threshold");
    let mut memo: HashMap<String, usize> = HashMap::new();
    fn chain_len(
        dag: &Dag,
        uid: &str,
        memo: &mut HashMap<String, usize>,
        visiting: &mut HashSet<String>,
    ) -> usize {
        if let Some(&len) = memo.get(uid) {
            return len;
        }
        let len = match dag.get(uid) {
            Some(n) if is_view_or_ephemeral(n) => {
                // A revisit mid-DFS would mean a cycle — the Dag can't have
                // one, so treat it as no chain rather than recurse forever.
                if !visiting.insert(uid.to_string()) {
                    return 0;
                }
                let best = dag
                    .direct_parents(uid)
                    .iter()
                    .map(|p| chain_len(dag, p, memo, visiting))
                    .max()
                    .unwrap_or(0);
                visiting.remove(uid);
                1 + best
            }
            _ => 0,
        };
        memo.insert(uid.to_string(), len);
        len
    }
    let mut visiting = HashSet::new();
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        let len = chain_len(dag, uid, &mut memo, &mut visiting);
        if len < threshold || !in_scope(n) {
            continue;
        }
        // Chain ENDS only — a view/ephemeral child means the chain runs on,
        // and a pruned package child means the end can't be confirmed.
        if dag
            .direct_children(uid)
            .iter()
            .any(|c| dag.get(c).is_some_and(is_view_or_ephemeral))
            || dag.pruned_adjacent(uid).is_some_and(|a| a.children > 0)
        {
            continue;
        }
        // Rebuild the longest chain: repeatedly take the parent with the
        // greatest chain length (ties → the smallest unique_id), then
        // reverse into upstream→downstream order.
        let mut chain = vec![(*uid).clone()];
        let mut cur = *uid;
        loop {
            let next = dag
                .direct_parents(cur)
                .iter()
                .map(|p| (chain_len(dag, p, &mut memo, &mut visiting), p))
                .filter(|(len, _)| *len > 0)
                .max_by(|(la, a), (lb, b)| la.cmp(lb).then_with(|| b.cmp(a)))
                .map(|(_, p)| p);
            let Some(next) = next else { break };
            chain.push(next.clone());
            cur = next;
        }
        chain.reverse();
        out.push(Finding::new(
            LintRule::ChainedViews,
            (*uid).clone(),
            chain.clone(),
            format!(
                "{} ends a chain of {len} consecutive {}: {}",
                n.name,
                pluralize(len, "view/ephemeral model"),
                name_list(&display_names(dag, &chain)),
            ),
        ));
    }
}

/// `exposure_parents_materializations`: an exposure directly depends on a
/// source or a view/ephemeral model — the last hop before a consumer should
/// be a durable model. One finding per exposure, `related` = the matching
/// parents (sorted).
pub(super) fn exposure_parents_materializations(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "exposure" || !in_scope(n) {
            continue;
        }
        let mut parents: Vec<String> = dag
            .direct_parents(uid)
            .iter()
            .filter(|p| {
                dag.get(p)
                    .is_some_and(|p| p.resource_type == "source" || is_view_or_ephemeral(p))
            })
            .cloned()
            .collect();
        if parents.is_empty() {
            continue;
        }
        parents.sort();
        out.push(Finding::new(
            LintRule::ExposureParentsMaterializations,
            (*uid).clone(),
            parents.clone(),
            format!(
                "exposure {} directly depends on source or view/ephemeral {}: {}",
                n.name,
                pluralize(parents.len(), "parent"),
                name_list(&display_names(dag, &parents)),
            ),
        ));
    }
}
