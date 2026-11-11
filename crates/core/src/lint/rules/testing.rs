//! `testing` — dbt test coverage rules.

use super::*;

/// `missing_primary_key_tests`: a data resource lacking `unique` +
/// `not_null` on the same column AND lacking a
/// `unique_combination_of_columns` test. The `resource_types` parameter
/// (default `[model]`) widens the check to snapshots and seeds.
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
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !types.iter().any(|t| t == &n.resource_type) || !in_scope(n) {
            continue;
        }
        // The ONE primary-key-guarantee predicate — shared with the TUI's
        // PrimaryKey lineage lens, so a node the lint clears can never be
        // tinted (and vice versa).
        if has_pk_guarantee(dag.tests(uid)) {
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
