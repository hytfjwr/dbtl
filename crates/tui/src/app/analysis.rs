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
        let report = crate::diff_report(
            &self.dag,
            self.diff_base.as_ref(),
            diff,
            self.diff_label(),
            self.scope(),
            &self.owner_meta_key,
        );
        Some(DiffView {
            baseline: report.baseline,
            scope: report.scope,
            added: report.added,
            removed: report.removed,
            modified: report.modified,
            edges_added: report.edges_added,
            edges_removed: report.edges_removed,
            column_changes: report.column_changes,
            pr: report.pr,
            scroll: 0,
        })
    }
    /// Compute the stats-dashboard payload from the `Dag` at open time, so the
    /// render layer never needs a `Dag` (mirrors [`crate::action::DetailView`]). Deterministic:
    /// counts accumulate into `BTreeMap`s and hubs are fully sorted (degree desc,
    /// then `unique_id` asc for a stable tie-break), so the result is bit-stable
    /// across runs despite the `HashMap` node iteration order.
    pub fn compute_stats_view(&self) -> StatsView {
        let mut by_rt: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_mat: BTreeMap<String, usize> = BTreeMap::new();
        let mut zero_down = 0;
        let mut no_desc = 0;
        let mut hubs: Vec<(String, String, usize)> = Vec::new();
        // Transitive blast-radius hubs (root-project membership like `hubs`;
        // the count itself is the graph-wide closure so the number matches the
        // DegreeHeat/impact readouts), and orphan models (zero kept parents
        // AND zero kept children).
        let mut transitive: Vec<(String, String, usize)> = Vec::new();
        let mut orphans: Vec<String> = Vec::new();
        // CLL v1: models that could not be parsed fell back to the
        // name-match approximation. Resolving here records each model's
        // fallback flag (the resolutions stay memoized for later browsing), so
        // the dashboard can report the same count `dbtl columns` prints.
        let mut cll_fallbacks = 0usize;
        // Coverage comes from the ONE shared source (`coverage_summary`, the
        // `coverage_gap` base) so the dashboard, the `t` lens, and the status
        // `cov` segment always quote the same numbers.
        let (testable_tested, testable_total) = self.coverage_summary();
        // Column-level coverage likewise: the SHARED `column_coverage_counts`
        // `dbtl docs`' README also quotes — same root-project testable base.
        let (col_tested, col_declared) = crate::column_coverage_counts(&self.dag);
        // Every count/list the dashboard reports describes the ROOT project —
        // the same [`Dag::is_root_project`] scope `coverage_counts` and
        // `dbtl check` share. Under the `All` package scope installed-package
        // nodes sit in the graph as dependency context but are never part of
        // your project's type/materialization/orphan/hub numbers. The owning
        // package is resolved ONCE for the sweep (`is_root_project`
        // re-resolves per call — an O(nodes) scan per node without recorded
        // metadata); the title-bar `model:N` counts deliberately do NOT scope
        // — see `compute_stats`.
        let primary = self.dag.primary_package();
        for (uid, n) in self.dag.nodes() {
            if !primary.as_deref().is_none_or(|p| crate::in_package(n, p)) {
                continue;
            }
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
                crate::ColumnResolver::resolve(&self.cll, &self.dag, uid);
                if self.cll.is_fallback(uid) {
                    cll_fallbacks += 1;
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
                let parent_name = self.dag.get(&p).map_or(p, |n| n.name.clone());
                let child_name = self.dag.get(&c).map_or(c, |n| n.name.clone());
                (parent_name, child_name)
            })
            .collect();
        layer_violations.sort();
        // The lint worklist from the shared `run_rules` findings (computed
        // once on the App): per-rule violating NODE NAMES, in LintRule::ALL
        // order, names sorted — the dashboard and `dbtl check --lint` quote
        // the same source, so they can never disagree.
        let lint_findings: Vec<(String, Vec<String>)> = crate::LintRule::ALL
            .iter()
            .filter_map(|rule| {
                let mut names: Vec<String> = self
                    .lint_findings()
                    .iter()
                    .filter(|f| f.rule == *rule)
                    .map(|f| {
                        self.dag
                            .get(&f.node_uid)
                            .map_or_else(|| f.node_uid.clone(), |n| n.name.clone())
                    })
                    .collect();
                names.sort();
                (!names.is_empty()).then(|| (rule.name().to_string(), names))
            })
            .collect();
        let critical_path = longest_chain(&self.dag);
        // The runtime-weighted critical path, only when run_results is loaded:
        // the chain that costs the most wall-clock time (unweighted sibling of
        // the depth chain — a 3-hop path of 60s models beats a 10-hop of 1s).
        let critical_path_runtime = self.run_results().map(|rr| {
            let (path, total) =
                weighted_chain(&self.dag, |uid| rr.execution_time(uid).unwrap_or(0.0));
            (path, format!("{total:.1}s"))
        });
        StatsView {
            project: self.stats.project.clone(),
            by_resource_type: by_rt.into_iter().collect(),
            by_materialization: by_mat.into_iter().collect(),
            testable_total,
            testable_tested,
            col_declared,
            col_tested,
            top_hubs: hubs,
            transitive_hubs,
            orphan_models: orphans,
            layer_violations,
            lint_findings,
            critical_path,
            critical_path_runtime,
            untested_testable: testable_total - testable_tested,
            zero_downstream_models: zero_down,
            no_description_models: no_desc,
            cll_fallbacks,
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

    /// The per-column `(upstream, downstream)` ONE-HOP column-edge counts for
    /// a node — the structure modal's `↑u ↓d` counts beside each column row
    /// (direct parents in / direct children out, per the contract).
    /// Drives the app-owned [`SqlParseResolver`](crate::SqlParseResolver)
    /// through a fresh lazy [`ColumnLineage`](crate::ColumnLineage) (the
    /// resolver's own per-uid memos persist; the engine's per-node memo is
    /// one-shot by design). Keyed by normalized column name; deterministic.
    pub fn column_counts_for(&self, uid: &str) -> BTreeMap<String, (usize, usize)> {
        crate::ColumnLineage::new(&self.dag, &self.cll).column_counts(uid)
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
    /// glyph mode's `Chrome` down/up badges (Unicode `↓`/`↑`,
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
    // Unweighted: every node costs 1, so (score, hops) degenerates to hops —
    // the same longest-path DP, same tie-breaks, the score just rides along.
    weighted_chain(dag, |_| 1.0).0
}

/// The heaviest-scored dependency chain in the DAG: a memoized DFS maximizing
/// `(total weight, hop count)` lexicographically — a heavy short chain beats a
/// cheap long one, and equal scores prefer MORE hops (a deeper chain is the
/// better "critical path" story). `weight` maps a `unique_id` to its cost
/// (the runtime path weighs nodes by `execution_time`, absent → 0.0).
///
/// Returns `(node-name path upstream→downstream, total weight)`. Tie-breaks
/// are deterministic: children iterate sorted (`Dag::edges` order), a strict
/// `>` keeps the first-best child, and the start is the smallest `unique_id`
/// achieving the max score. Same cycle guard as the unweighted version —
/// a back-edge target counts as a leaf so malformed cyclic manifests still
/// terminate.
pub(super) fn weighted_chain(dag: &Dag, weight: impl Fn(&str) -> f64) -> (Vec<String>, f64) {
    use std::collections::{HashMap, HashSet};
    let edges = dag.edges();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (p, c) in &edges {
        children.entry(p.as_str()).or_default().push(c.as_str());
    }

    // ((total weight, hop count) from uid, the next hop on that chain).
    fn chain<'a>(
        uid: &'a str,
        children: &HashMap<&'a str, Vec<&'a str>>,
        weight: &dyn Fn(&str) -> f64,
        memo: &mut HashMap<&'a str, ((f64, usize), Option<&'a str>)>,
        on_stack: &mut HashSet<&'a str>,
    ) -> ((f64, usize), Option<&'a str>) {
        if let Some(&hit) = memo.get(uid) {
            return hit;
        }
        // A back-edge into the live DFS stack (a cycle) would recurse forever:
        // break it by treating the re-entered node as a leaf. NOT memoized —
        // the node's real value is finished by its own frame further up.
        if !on_stack.insert(uid) {
            return ((weight(uid), 1), None);
        }
        let w = weight(uid);
        let mut best = ((w, 1), None);
        for &kid in children.get(uid).into_iter().flatten() {
            let ((kw, kl), _) = chain(kid, children, weight, memo, on_stack);
            let cand = (w + kw, kl + 1);
            // Strictly greater: kids come sorted, so ties keep the first.
            if cand.0 > best.0 .0 || (cand.0 == best.0 .0 && cand.1 > best.0 .1) {
                best = (cand, Some(kid));
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
    // Compute every start's score, then take the FIRST maximum (sorted order
    // is ascending uid → smallest-uid winner, deterministic).
    let mut best_start: Option<&str> = None;
    let mut best_score = (0.0f64, 0usize);
    for &uid in &starts {
        let score = chain(uid, &children, &weight, &mut memo, &mut on_stack).0;
        let better = match best_start {
            None => true,
            Some(_) => {
                score.0 > best_score.0 || (score.0 == best_score.0 && score.1 > best_score.1)
            }
        };
        if better {
            best_start = Some(uid);
            best_score = score;
        }
    }
    let Some(start) = best_start else {
        return (Vec::new(), 0.0);
    };

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
    (path, best_score.0)
}

/// Compute the title-bar stats from a `Dag`. The project name is the
/// manifest's recorded root project (`metadata.project_name`), falling back
/// to a `unique_id`-segment heuristic — resolved by [`Dag::project_name`] so
/// the title bar and the docs index quote the same name.
///
/// The resource counts intentionally describe the LOADED graph — every node
/// the list and lineage can show — so under `--include-packages` the
/// `model:N`/`src:N`… chips include installed-package context while the `S`
/// dashboard, coverage, and `dbtl check` still count the root project only.
/// The divergence is deliberate: the title enumerates what is on screen, the
/// dashboard reports your project's health.
pub(super) fn compute_stats(dag: &Dag) -> AppStats {
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
