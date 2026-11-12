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

/// `references_deprecated_model`: a node directly depends on a model
/// carrying `deprecation_date` — the upstream's scheduled retirement.
/// Every resource kind that CAN parent a model is checked (models,
/// snapshots, exposures, the semantic-layer resources); the deprecated
/// model itself never flags — the finding names the edge, not the
/// sunset. The date is read as declared text only (never parsed — the
/// judgment is "a deprecation is declared", not "the cutoff passed"), and
/// a blank string is no declaration at all. The position lands on the
/// dependent's first `ref()` to a deprecated parent; every other
/// deprecated-parent call is a label.
pub(super) fn references_deprecated_model(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sites: &CallSiteResolver<'_>,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !matches!(
            n.resource_type.as_str(),
            "model" | "snapshot" | "exposure" | "semantic_model" | "metric" | "saved_query"
        ) || !in_scope(n)
        {
            continue;
        }
        let mut parents: Vec<String> = dag
            .direct_parents(uid)
            .iter()
            .filter(|p| {
                dag.get(p).is_some_and(|p| p.resource_type == "model")
                    && dag
                        .detail(p)
                        .and_then(|d| d.deprecation_date.as_deref())
                        .is_some_and(|s| !s.trim().is_empty())
            })
            .cloned()
            .collect();
        if parents.is_empty() {
            continue;
        }
        parents.sort();
        let names: Vec<String> = parents
            .iter()
            .map(|p| {
                format!(
                    "{} (deprecated on {})",
                    display_name(dag, p),
                    dag.detail(p)
                        .and_then(|d| d.deprecation_date.as_deref())
                        .expect("filtered above")
                )
            })
            .collect();
        let mut f = Finding::new(
            LintRule::ReferencesDeprecatedModel,
            (*uid).clone(),
            parents.clone(),
            format!("{} depends on {}", n.name, name_list(&names)),
        );
        // Primary on the first ref to a deprecated parent; every other
        // deprecated-parent call is a label naming what it reads.
        if let Some(cs) = sites.for_node(uid) {
            let mut spots: Vec<(std::ops::Range<usize>, String)> = Vec::new();
            for parent in &parents {
                for span in cs.calls_to(parent) {
                    spots.push((span, format!("{} is deprecated", display_name(dag, parent))));
                }
            }
            spots.sort_by_key(|(r, _)| r.start);
            if let Some(first) = spots.first().cloned() {
                f = cs.annotate(f, first, spots[1..].to_vec());
            }
        }
        out.push(f);
    }
}
