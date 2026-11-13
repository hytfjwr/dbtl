//! The coverage/counts family: `(x, total)` pairs over the root project's
//! testable/documentable base — model test coverage, declared-column test
//! coverage, model description coverage, and column description coverage —
//! plus the [`model_documented`] predicate the `doc_coverage` gate and the
//! `undocumented_models` lint rule share. Each counter is the ONE source its
//! surfaces read (status `cov` segment, `t` lens, stats dashboard, `dbtl
//! docs` summaries, `dbtl check` gates), so they can never disagree.

use std::collections::{BTreeSet, HashSet};

use crate::dag::testable;
use crate::{coverage_gap, in_package, ColumnInfo, Dag, NodeDetail};

/// The `(tested, total)` coverage pair over buildable nodes
/// (model / snapshot / seed): `total` counts them all, `tested` counts those
/// with at least one test — the ONE coverage source shared by the status `cov`
/// segment, the `t` lens, the stats dashboard, `dbtl-docs`' summary, and
/// `dbtl check --coverage-min`.
///
/// Always counted over the ROOT PROJECT ONLY ([`Dag::is_root_project`]): under
/// the `All` package scope installed-package nodes exist in the graph but are
/// never part of your project's test coverage. Under the default `Root` scope
/// (or a single-package manifest) the predicate passes every node, so this is
/// the exact count it always was. `within` further narrows the counted set to
/// a `--select` uid set (`None` = the whole project — the behavior every
/// pre-selection caller keeps).
pub fn coverage_counts(dag: &Dag, within: Option<&BTreeSet<String>>) -> (usize, usize) {
    // Resolve the owning package ONCE for the sweep — `is_root_project`
    // re-resolves it per call, an O(nodes) scan per node when no
    // `metadata.project_name` is recorded.
    let primary = dag.primary_package();
    let mut total = 0;
    let mut tested = 0;
    for n in dag.nodes().values() {
        if !primary.as_deref().is_none_or(|p| in_package(n, p)) {
            continue;
        }
        if within.is_some_and(|s| !s.contains(&n.unique_id)) {
            continue;
        }
        // `testable` is the counted universe; `coverage_gap` its untested
        // half — the same predicates the Coverage lens reads.
        if testable(n) {
            total += 1;
            if !coverage_gap(n) {
                tested += 1;
            }
        }
    }
    (tested, total)
}

/// The `(covered, declared)` COLUMN-coverage pair, over the same testable base
/// as [`coverage_counts`] (model / snapshot / seed, root project): `declared`
/// counts every entry in the nodes' [`crate::NodeDetail::columns`] lists,
/// `covered` counts the entries at least one attached test names
/// (`test.column_name`, both sides compared through
/// [`normalize_column`](crate::normalize_column)). A node with zero declared
/// columns contributes nothing — the metric answers "of the columns the
/// project actually documents, how many are test-guarded" (columns with
/// ≥1 test / declared columns).
///
/// The ONE source for the stats dashboard's column gauge and `dbtl docs`'s
/// README bullet, so the two surfaces can never disagree — the same shape
/// [`coverage_counts`] gives the row-level coverage readouts.
///
/// Caveat: source mode fills seed columns from the CSV header (a documented
/// superset of schema-declared columns), so `declared` can read slightly
/// larger there than on the same project's compiled manifest — the same
/// documented divergence the `columns` consistency assert already carries.
pub fn column_coverage_counts(dag: &Dag, within: Option<&BTreeSet<String>>) -> (usize, usize) {
    let primary = dag.primary_package();
    let mut declared = 0;
    let mut covered = 0;
    for n in dag.nodes().values() {
        if !primary.as_deref().is_none_or(|p| in_package(n, p)) {
            continue;
        }
        if within.is_some_and(|s| !s.contains(&n.unique_id)) {
            continue;
        }
        if !testable(n) {
            continue;
        }
        let Some(d) = dag.detail(&n.unique_id) else {
            continue;
        };
        if d.columns.is_empty() {
            continue;
        }
        let tested: HashSet<String> = dag
            .tests(&n.unique_id)
            .iter()
            .filter_map(|t| t.column_name.as_deref())
            .map(crate::normalize_column)
            .collect();
        declared += d.columns.len();
        covered += d
            .columns
            .iter()
            .filter(|c| tested.contains(crate::normalize_column(c.name.as_str()).as_str()))
            .count();
    }
    (covered, declared)
}

/// Whether a node's model-level description counts as documented for the
/// `doc_coverage` gate — `Some(_)`, the SAME predicate the
/// `undocumented_models` lint rule fires on (one definition so the gate and
/// the linter can never disagree on what "documented" means for a model).
pub fn model_documented(detail: &NodeDetail) -> bool {
    detail.description.is_some()
}

/// Whether a declared column counts as documented — a description that is
/// present AND non-blank: a whitespace-only `description: " "` counts as
/// missing. The loaders already normalize a blank column description to
/// `None` via the shared [`crate::dag::description_blank`] predicate, so the
/// `undocumented_columns` lint rule's `is_none()` lands on the same set —
/// this predicate stays for the hand-built-manifest edge a loader never saw.
fn column_documented(col: &ColumnInfo) -> bool {
    col.description
        .as_deref()
        .is_some_and(|d| !crate::dag::description_blank(d))
}

/// The `(documented, total)` description-coverage pair over the documentable
/// base — ROOT-PROJECT `model` nodes only (the set `dbt docs generate` can
/// render a description for; a seed's "description" is its CSV). `total`
/// counts them all; `documented` counts those carrying a description
/// ([`model_documented`]). Sits beside [`coverage_counts`]/
/// [`column_coverage_counts`]: the same root-project scoping and the same
/// "of the things the project can document, how many are documented" shape,
/// so `dbtl check`'s `doc_coverage` gate reads the same numbers any other
/// surface would.
pub fn doc_coverage_counts(dag: &Dag, within: Option<&BTreeSet<String>>) -> (usize, usize) {
    let primary = dag.primary_package();
    let mut total = 0;
    let mut documented = 0;
    for n in dag.nodes().values() {
        if !primary.as_deref().is_none_or(|p| in_package(n, p)) {
            continue;
        }
        if within.is_some_and(|s| !s.contains(&n.unique_id)) {
            continue;
        }
        if n.resource_type != "model" {
            continue;
        }
        total += 1;
        if dag.detail(&n.unique_id).is_some_and(model_documented) {
            documented += 1;
        }
    }
    (documented, total)
}

/// The `(documented, declared)` COLUMN-description pair, over the same model
/// base as [`doc_coverage_counts`] (root project, `model`): `declared`
/// counts every entry in the nodes' `columns` lists, `documented` counts the
/// entries carrying a real description (`column_documented` —
/// whitespace-only doesn't count). A node with zero declared columns
/// contributes nothing — the same "of the columns the project declares"
/// framing the test-coverage metric uses.
pub fn column_doc_coverage_counts(dag: &Dag, within: Option<&BTreeSet<String>>) -> (usize, usize) {
    let primary = dag.primary_package();
    let mut declared = 0;
    let mut documented = 0;
    for n in dag.nodes().values() {
        if !primary.as_deref().is_none_or(|p| in_package(n, p)) {
            continue;
        }
        if within.is_some_and(|s| !s.contains(&n.unique_id)) {
            continue;
        }
        if n.resource_type != "model" {
            continue;
        }
        let Some(d) = dag.detail(&n.unique_id) else {
            continue;
        };
        declared += d.columns.len();
        documented += d.columns.iter().filter(|c| column_documented(c)).count();
    }
    (documented, declared)
}
