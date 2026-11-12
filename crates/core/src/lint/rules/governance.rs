//! `governance` — access/contract conventions and required annotations.

use super::*;

/// `public_models_without_contract`: `access: public` with no enforced
/// contract — a model claiming a stable public interface without the
/// warehouse enforcing it.
pub(super) fn public_models_without_contract(
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
        if d.access.as_deref() != Some("public") || d.contract_enforced {
            continue;
        }
        out.push(Finding::new(
            LintRule::PublicModelsWithoutContract,
            (*uid).clone(),
            Vec::new(),
            format!("{} is access: public without an enforced contract", n.name),
        ));
    }
}

/// `exposures_dependent_on_private_models`: an exposure whose direct model
/// parents declare `access: private`/`protected`. `access: None` means
/// "unknown" — reachable only on a manifest whose wire genuinely omits
/// `access` (source mode fills dbt's `protected` model default) — so
/// `None` is never judged.
pub(super) fn exposures_dependent_on_private_models(
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
                dag.get(p).is_some_and(|p| {
                    p.resource_type == "model"
                        && dag
                            .detail(&p.unique_id)
                            .and_then(|d| d.access.as_deref())
                            .is_some_and(|a| a != "public")
                })
            })
            .cloned()
            .collect();
        if parents.is_empty() {
            continue;
        }
        parents.sort();
        out.push(Finding::new(
            LintRule::ExposuresDependentOnPrivateModels,
            (*uid).clone(),
            parents.clone(),
            format!(
                "exposure {} directly depends on non-public {}: {}",
                n.name,
                pluralize(parents.len(), "model"),
                name_list(&display_names(dag, &parents)),
            ),
        ));
    }
}

/// `required_meta_keys`: a model missing any configured `meta` key — the
/// `keys` parameter, empty by default (an empty list makes the rule a
/// no-op). Values aren't inspected: a key is present even when its value is
/// the empty string.
pub(super) fn required_meta_keys(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let keys = config
        .str_list_param(LintRule::RequiredMetaKeys, "keys")
        .expect("the registry declares required_meta_keys.keys");
    if keys.is_empty() {
        return;
    }
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let Some(d) = dag.detail(uid) else { continue };
        let missing: Vec<&str> = keys
            .iter()
            .filter(|k| !d.meta.contains_key(k.as_str()))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            continue;
        }
        out.push(Finding::new(
            LintRule::RequiredMetaKeys,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{} is missing required meta {}: {}",
                n.name,
                pluralize(missing.len(), "key"),
                missing.join(", ")
            ),
        ));
    }
}

/// `contract_columns_without_type`: an enforced-contract model with declared
/// columns that lack `data_type` — a contract is only as strong as the
/// column definitions behind it.
pub(super) fn contract_columns_without_type(
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
        if !d.contract_enforced {
            continue;
        }
        let columns: Vec<&str> = d
            .columns
            .iter()
            .filter(|c| c.data_type.is_none())
            .map(|c| c.name.as_str())
            .collect();
        if columns.is_empty() {
            continue;
        }
        out.push(Finding::new(
            LintRule::ContractColumnsWithoutType,
            (*uid).clone(),
            Vec::new(),
            format!(
                "contract-enforced {} has declared {} without data_type: {}",
                n.name,
                pluralize(columns.len(), "column"),
                columns.join(", ")
            ),
        ));
    }
}
