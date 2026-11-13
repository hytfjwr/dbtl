//! `documentation` — description and doc-coverage rules.

use super::*;

/// `undocumented_models`: a model with no description — the
/// [`crate::model_documented`] predicate the `doc_coverage` gate counts by
/// (`clean_desc` already normalizes whitespace-only to `None` at build).
/// The `resource_types` parameter (default `[model]`) widens the check to
/// seeds and snapshots.
pub(super) fn undocumented_models(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let types = config
        .str_list_param(LintRule::UndocumentedModels, "resource_types")
        .expect("the registry declares undocumented_models.resource_types");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !types.iter().any(|t| t == &n.resource_type) || !in_scope(n) {
            continue;
        }
        if dag.detail(uid).is_some_and(crate::model_documented) {
            continue;
        }
        out.push(Finding::new(
            LintRule::UndocumentedModels,
            (*uid).clone(),
            Vec::new(),
            format!("{} has no description", display_name(dag, uid)),
        ));
    }
}

/// `undocumented_sources`: a source (table) with no description — the same
/// `clean_desc`-normalized check [`undocumented_models`] runs, at the DAG's
/// upstream edge.
pub(super) fn undocumented_sources(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "source" || !in_scope(n) {
            continue;
        }
        if dag
            .detail(uid)
            .and_then(|d| d.description.as_ref())
            .is_some()
        {
            continue;
        }
        out.push(Finding::new(
            LintRule::UndocumentedSources,
            (*uid).clone(),
            Vec::new(),
            format!("source {} has no description", display_name(dag, uid)),
        ));
    }
}

/// `undocumented_exposures`: an exposure with no description — the same
/// `clean_desc`-normalized check [`undocumented_models`] runs, at the DAG's
/// downstream edge. Opt-in (`default_enabled: false`) — dbt never required
/// a description, so most projects' exposures all flag until documented or
/// the rule stays off.
pub(super) fn undocumented_exposures(
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
        if dag
            .detail(uid)
            .and_then(|d| d.description.as_ref())
            .is_some()
        {
            continue;
        }
        out.push(Finding::new(
            LintRule::UndocumentedExposures,
            (*uid).clone(),
            Vec::new(),
            format!("exposure {} has no description", display_name(dag, uid)),
        ));
    }
}

/// `undocumented_columns`: a node whose declared columns include at least
/// one without a description — one finding per node naming the bare columns
/// (a node with no declared columns is not "missing column descriptions").
/// The `resource_types` parameter (default `[model]`) widens the check to
/// seeds, snapshots, and sources — a source finding's name chain lands on
/// the `[source, table, column]` path (see `location.rs::name_chain`).
pub(super) fn undocumented_columns(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let types = config
        .str_list_param(LintRule::UndocumentedColumns, "resource_types")
        .expect("the registry declares undocumented_columns.resource_types");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !types.iter().any(|t| t == &n.resource_type) || !in_scope(n) {
            continue;
        }
        let Some(d) = dag.detail(uid) else { continue };
        let columns: Vec<&str> = d
            .columns
            .iter()
            .filter(|c| c.description.is_none())
            .map(|c| c.name.as_str())
            .collect();
        if columns.is_empty() {
            continue;
        }
        out.push(Finding::new(
            LintRule::UndocumentedColumns,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{} has declared {} without descriptions: {}",
                display_name(dag, uid),
                pluralize(columns.len(), "column"),
                columns.join(", ")
            ),
        ));
    }
}
