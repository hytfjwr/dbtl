//! Graph analytics over the `Dag`: blast radius (impact), test coverage, the
//! stats dashboard payload, and the critical path. The PR-impact analysis
//! (`pr_impact` / `diff_report` / `layer_violation_edges` /
//! `PR_HUB_MIN_DOWNSTREAM` / `coverage_counts` / `exposure_note` /
//! `pr_impact_markdown`) lives in `dbtl_core::impact` so `dbtl impact` and
//! `dbtl check` share it — the TUI reads it through the `crate::` re-export.

use std::collections::BTreeMap;

use crate::action::{DiffView, StatsView};
use crate::{Dag, NodeInfo};

use super::{App, AppStats};

impl App {
    /// Snapshot the baseline diff into the `D` modal's payload — the shared
    /// [`dbtl_core::diff_report`](crate::diff_report) data plus the modal's
    /// scroll offset. `None` without a `--diff` baseline — the reducer toasts
    /// a hint instead of opening.
    pub fn compute_diff_view(&self) -> Option<DiffView> {
        let diff = self.diff()?;
        let report =
            crate::diff_report(&self.dag, self.diff_base.as_ref(), diff, self.diff_label());
        Some(DiffView {
            baseline: report.baseline,
            added: report.added,
            removed: report.removed,
            modified: report.modified,
            edges_added: report.edges_added,
            edges_removed: report.edges_removed,
            pr: report.pr,
            scroll: 0,
        })
    }
    /// Compute the stats-dashboard payload from the `Dag` at open time, so the
    /// render layer never needs a `Dag` (mirrors [`DetailView`]). Deterministic:
    /// counts accumulate into `BTreeMap`s and hubs are fully sorted (degree desc,
    /// then `unique_id` asc for a stable tie-break), so the result is bit-stable
    /// across runs despite the `HashMap` node iteration order.
    pub fn compute_stats_view(&self) -> StatsView {
        let mut by_rt: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_mat: BTreeMap<String, usize> = BTreeMap::new();
        let mut zero_down = 0;
        let mut no_desc = 0;
        let mut hubs: Vec<(String, String, usize)> = Vec::new();
        // Transitive blast-radius hubs over ALL nodes (same base as `hubs`), and
        // orphan models (zero kept parents AND zero kept children).
        let mut transitive: Vec<(String, String, usize)> = Vec::new();
        let mut orphans: Vec<String> = Vec::new();
        // Coverage comes from the ONE shared source (`coverage_summary`, the
        // `coverage_gap` base) so the dashboard, the `t` lens, and the status
        // `cov` segment always quote the same numbers.
        let (testable_tested, testable_total) = self.coverage_summary();
        for (uid, n) in self.dag.nodes() {
            *by_rt.entry(n.resource_type.clone()).or_default() += 1;
            let degree = n.direct_up + n.direct_down;
            hubs.push((uid.clone(), n.name.clone(), degree));
            transitive.push((uid.clone(), n.name.clone(), self.dag.downstream(uid).len()));
            if n.resource_type == "model" {
                let mat = n.materialized.clone().unwrap_or_else(|| "(none)".into());
                *by_mat.entry(mat).or_default() += 1;
                if n.direct_down == 0 {
                    zero_down += 1;
                }
                if n.direct_up == 0 && n.direct_down == 0 {
                    orphans.push(n.name.clone());
                }
                let has_desc = self
                    .dag
                    .detail(uid)
                    .and_then(|d| d.description.as_ref())
                    .is_some();
                if !has_desc {
                    no_desc += 1;
                }
            }
        }
        // Top-5 by degree desc, then unique_id asc for a deterministic tie-break.
        hubs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        hubs.truncate(5);
        // Top-5 by transitive-downstream closure desc, unique_id asc tie-break;
        // project to (name, count) AFTER sorting (the tie-break key is the uid).
        transitive.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        transitive.truncate(5);
        let transitive_hubs: Vec<(String, usize)> = transitive
            .into_iter()
            .map(|(_, name, c)| (name, c))
            .collect();
        orphans.sort();
        // Reuse the single layer-violation predicate; map uids → names, sort.
        let mut layer_violations: Vec<(String, String)> = crate::layer_violation_edges(&self.dag)
            .into_iter()
            .map(|(p, c)| {
                let pn = self.dag.get(&p).map_or(p, |n| n.name.clone());
                let cn = self.dag.get(&c).map_or(c, |n| n.name.clone());
                (pn, cn)
            })
            .collect();
        layer_violations.sort();
        let critical_path = longest_chain(&self.dag);
        StatsView {
            project: self.stats.project.clone(),
            by_resource_type: by_rt.into_iter().collect(),
            by_materialization: by_mat.into_iter().collect(),
            testable_total,
            testable_tested,
            top_hubs: hubs,
            transitive_hubs,
            orphan_models: orphans,
            layer_violations,
            critical_path,
            untested_testable: testable_total - testable_tested,
            zero_downstream_models: zero_down,
            no_description_models: no_desc,
            scroll: 0,
        }
    }

    /// The blast radius of the current ROOT (selected) node as
    /// `(downstream_count, upstream_count)`: the sizes of its transitive
    /// descendant / ancestor closures. Both come from the `Dag` closure methods,
    /// which already EXCLUDE the node itself, so the node is never double-counted.
    /// Order-independent (just `.len()` over a `HashSet`), hence deterministic.
    /// `None` when nothing is selected.
    pub fn impact_counts(&self) -> Option<(usize, usize)> {
        self.selected_unique_id().map(|uid| {
            let (down, up, _) = self.impact_breakdown_cached(&uid);
            (down, up)
        })
    }

    /// The blast radius of an arbitrary node as `(downstream_count,
    /// upstream_count)`. Shared by [`impact_counts`](App::impact_counts) (root) and
    /// the structure modal (which targets the focus uid, NOT necessarily the root —
    /// the lineage cursor can sit on a non-root source/seed/snapshot). Exposures
    /// are EXCLUDED from `downstream_count` (the two-number surface drops their
    /// split); use [`impact_breakdown_for`](App::impact_breakdown_for) when the
    /// caller also needs the exposure count.
    pub fn impact_counts_for(&self, uid: &str) -> (usize, usize) {
        let (down, up, _) = self.impact_breakdown_for(uid);
        (down, up)
    }

    /// The full blast-radius breakdown of a node: `(downstream count WITHOUT
    /// exposures, upstream count, downstream exposure count)`. Exposures are
    /// split out so "downstream" keeps meaning *buildable* resources (the
    /// pre-exposure numbers every frozen assertion pins) while the "who cares"
    /// half gets its own count. One closure walk computes all three; the
    /// upstream side needs no split — exposures have no children, so they can
    /// never be anyone's ancestor.
    pub fn impact_breakdown_for(&self, uid: &str) -> (usize, usize, usize) {
        let down = self.dag.downstream(uid);
        let exposures = down
            .iter()
            .filter(|id| self.dag.get(id).is_some_and(crate::is_exposure))
            .count();
        (
            down.len() - exposures,
            self.dag.upstream(uid).len(),
            exposures,
        )
    }

    /// The downstream exposures of a node, as `(info, payload)` pairs sorted by
    /// name (then uid — names are unique per project, but stay deterministic
    /// anyway). The impact report's "Affected exposures" section and the status
    /// chip's `exp:N` both derive from this set.
    pub fn downstream_exposures(
        &self,
        uid: &str,
    ) -> Vec<(&NodeInfo, Option<&crate::ExposureInfo>)> {
        let mut out: Vec<&NodeInfo> = self
            .dag
            .downstream(uid)
            .into_iter()
            .filter_map(|id| self.dag.get(&id))
            .filter(|n| crate::is_exposure(n))
            .collect();
        out.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.unique_id.cmp(&b.unique_id))
        });
        out.into_iter()
            .map(|n| (n, self.dag.exposure(&n.unique_id)))
            .collect()
    }

    /// The "impact ↓D ↑U" status segment for the selected node, using the active
    /// glyph mode's [`Chrome`](crate::ui::Chrome) down/up badges (Unicode `↓`/`↑`,
    /// ASCII `v`/`^`) so the arrows are never hardcoded (the ascii-guard contract).
    /// Gains an ` exp:N` suffix when N downstream exposures exist (the "who
    /// cares" half of the impact question); 0 stays silent so the pre-exposure
    /// chip is byte-identical. `None` when nothing is selected. Mirrors
    /// [`lineage_view_label`](App::lineage_view_label)'s role: the one place this
    /// string is built, reused by the loop and the ascii-guard render test.
    pub fn impact_status(&self) -> Option<String> {
        let uid = self.selected_unique_id()?;
        let (down, up, exposures) = self.impact_breakdown_cached(&uid);
        let chrome = crate::ui::chrome(self.glyph_mode);
        let mut out = format!(
            "impact {d}{down} {u}{up}",
            d = chrome.badge_down,
            u = chrome.badge_up
        );
        if exposures > 0 {
            out.push_str(&format!(" exp:{exposures}"));
        }
        Some(out)
    }

    /// `(tested, total)` over the project's testable resources (the
    /// [`coverage_gap`](crate::coverage_gap) base: model / snapshot / seed).
    /// Delegates to [`dbtl_core::coverage_counts`](crate::coverage_counts), the
    /// ONE coverage source — the status `cov` segment, the `t` lens, the stats
    /// dashboard, and `dbtl check --coverage-min` can never disagree.
    pub fn coverage_summary(&self) -> (usize, usize) {
        crate::coverage_counts(&self.dag)
    }
}

/// The longest dependency chain in the DAG as node NAMES, upstream →
/// downstream. Memoized DFS over `Dag::edges()` (sorted, so children are
/// visited in `unique_id` order); ties at every level keep the first (smallest
/// uid) candidate, making the result fully deterministic. dbt manifests are
/// acyclic by construction, but a malformed one can carry a cycle that
/// survives the prune (only test/operation nodes are dropped), so the DFS
/// keeps an on-stack set and treats a back-edge target as a leaf — the
/// recursion terminates instead of overflowing the stack. On an acyclic
/// manifest the guard never fires, so the result is unchanged.
pub(super) fn longest_chain(dag: &Dag) -> Vec<String> {
    use std::collections::{HashMap, HashSet};
    let edges = dag.edges();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (p, c) in &edges {
        children.entry(p.as_str()).or_default().push(c.as_str());
    }

    // (chain length from uid, the next hop on that chain).
    fn chain<'a>(
        uid: &'a str,
        children: &HashMap<&'a str, Vec<&'a str>>,
        memo: &mut HashMap<&'a str, (usize, Option<&'a str>)>,
        on_stack: &mut HashSet<&'a str>,
    ) -> (usize, Option<&'a str>) {
        if let Some(&hit) = memo.get(uid) {
            return hit;
        }
        // A back-edge into the live DFS stack (a cycle) would recurse forever:
        // break it by treating the re-entered node as a leaf. NOT memoized —
        // the node's real value is finished by its own frame further up.
        if !on_stack.insert(uid) {
            return (1, None);
        }
        let mut best = (1, None);
        for &kid in children.get(uid).into_iter().flatten() {
            let (len, _) = chain(kid, children, memo, on_stack);
            // Strictly greater: kids come sorted, so ties keep the first.
            if len + 1 > best.0 {
                best = (len + 1, Some(kid));
            }
        }
        on_stack.remove(uid);
        memo.insert(uid, best);
        best
    }

    let mut memo = HashMap::new();
    let mut on_stack = HashSet::new();
    let mut starts: Vec<&str> = dag.nodes().keys().map(String::as_str).collect();
    starts.sort_unstable();
    let Some(&start) = starts
        .iter()
        .max_by_key(|uid| chain(uid, &children, &mut memo, &mut on_stack).0)
    else {
        return Vec::new();
    };
    // max_by_key keeps the LAST maximum; re-scan for the FIRST (smallest uid).
    let best_len = memo[start].0;
    let start = *starts
        .iter()
        .find(|uid| memo[**uid].0 == best_len)
        .expect("a maximum exists");

    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = Some(start);
    while let Some(uid) = cur {
        // Under a cycle the memoized next-hops can point back into the chain;
        // stop at the first repeat so the walk terminates (acyclic: no-op).
        if !seen.insert(uid) {
            break;
        }
        path.push(
            dag.get(uid)
                .map_or_else(|| uid.to_string(), |n| n.name.clone()),
        );
        cur = memo[uid].1;
    }
    path
}

/// Compute the title-bar stats from a `Dag`. The project name is the second
/// segment of any node's `unique_id` (`model.<project>.<name>`), which is the
/// same across a single dbt project.
pub(super) fn compute_stats(dag: &Dag) -> AppStats {
    // The shared project-name derivation (deterministic: the
    // lexicographically-smallest `unique_id` project segment) lives on `Dag`,
    // so the title bar and the docs index quote the same name.
    let project = dag.project_name().unwrap_or("").to_string();
    AppStats {
        project,
        models: dag.count_by_resource_type("model"),
        sources: dag.count_by_resource_type("source"),
        seeds: dag.count_by_resource_type("seed"),
        snapshots: dag.count_by_resource_type("snapshot"),
        exposures: dag.count_by_resource_type("exposure"),
    }
}
