//! `testing` — dbt test coverage rules.

use super::*;

/// A declared `primary_key` constraint — column-level (`constraints:` on
/// the column) or model-level (the `constraints:` list's `type:
/// primary_key`). Most warehouses never ENFORCE it (a Snowflake/
/// BigQuery PK is metadata, not a guarantee), so it only substitutes for
/// tests under the `accept_primary_key_constraint` opt-in.
fn has_pk_constraint(d: &crate::NodeDetail) -> bool {
    d.constraints
        .iter()
        .any(|c| c.constraint_type == "primary_key")
        || d.columns.iter().any(|c| {
            c.constraints
                .iter()
                .any(|k| k.constraint_type == "primary_key")
        })
}

/// `missing_primary_key_tests`: a data resource lacking `unique` +
/// `not_null` on the same column AND lacking a
/// `unique_combination_of_columns` test. The `resource_types` parameter
/// (default `[model]`) widens the check to snapshots and seeds;
/// `accept_primary_key_constraint` (default `false`) additionally accepts
/// a declared `primary_key` constraint as the guarantee.
pub(super) fn missing_primary_key_tests(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let types = config
        .str_list_param(LintRule::MissingPrimaryKeyTests, "resource_types")
        .expect("the registry declares missing_primary_key_tests.resource_types");
    let accept_pk = config
        .bool_param(
            LintRule::MissingPrimaryKeyTests,
            "accept_primary_key_constraint",
        )
        .expect("the registry declares missing_primary_key_tests.accept_primary_key_constraint");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !types.iter().any(|t| t == &n.resource_type) || !in_scope(n) {
            continue;
        }
        // The ONE primary-key-guarantee predicate — shared with the TUI's
        // PrimaryKey lineage lens, so a node the lint clears can never be
        // tinted (and vice versa). The constraint opt-in sits OUTSIDE it
        // on purpose: the lens' "documented" judgment stays constraint-
        // free regardless of lint config.
        if has_pk_guarantee(dag.tests(uid)) {
            continue;
        }
        if accept_pk && dag.detail(uid).is_some_and(has_pk_constraint) {
            continue;
        }
        out.push(Finding::new(
            LintRule::MissingPrimaryKeyTests,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{} has no unique+not_null test pair (or unique_combination_of_columns)",
                n.name
            ),
        ));
    }
}

/// `sources_without_freshness`: a source with no effective freshness — the
/// [`NodeDetail::has_freshness`] predicate (a `warn_after`/`error_after`
/// with a count after dbt's source→table merge and `null` clearing).
pub(super) fn sources_without_freshness(
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
        if dag.detail(uid).is_some_and(|d| d.has_freshness()) {
            continue;
        }
        out.push(Finding::new(
            LintRule::SourcesWithoutFreshness,
            (*uid).clone(),
            Vec::new(),
            format!(
                "source {} has no freshness configured",
                display_name(dag, uid)
            ),
        ));
    }
}
