//! `documentation` — description and doc-coverage rules.

use super::*;

/// `undocumented_models`: a model with no description (`clean_desc` already
/// normalizes whitespace-only to `None` at build).
pub(super) fn undocumented_models(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
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
            LintRule::UndocumentedModels,
            (*uid).clone(),
            Vec::new(),
            format!("{} has no description", n.name),
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
            format!("source {} has no description", n.name),
        ));
    }
}

/// `undocumented_columns`: a model whose declared columns include at least
/// one without a description — one finding per model naming the bare columns
/// (a model with no declared columns is not "missing column descriptions").
pub(super) fn undocumented_columns(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
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
                "{} has declared column(s) without descriptions: {}",
                n.name,
                columns.join(", ")
            ),
        ));
    }
}
