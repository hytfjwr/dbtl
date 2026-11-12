//! `structure` — naming and file-layout conventions.

use super::*;

/// `naming_conventions`: a model whose [`lint_model_type`] is listed in
/// `naming_prefixes` must start with one of that type's prefixes. The type is
/// resolved prefix-first — `staging/fct_x` is a *marts*
/// model (not a misnamed staging one) and `reports/x` is `other`, which is
/// prefix-free unless the config defines an `other:` layer.
pub(super) fn naming_conventions(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let layer = lint_model_type(n, config);
        let Some(prefixes) = config.naming_prefixes.get(layer) else {
            continue;
        };
        if prefixes.is_empty() || prefixes.iter().any(|p| n.name.starts_with(p.as_str())) {
            continue;
        }
        out.push(Finding::new(
            LintRule::NamingConventions,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{layer} model {} doesn't match prefix(es): {}",
                n.name,
                prefixes.join(", ")
            ),
        ));
    }
}

/// `model_directories`: the prefix-derived layer
/// ([`lint_model_type_by_prefix`]) and the directory-derived layer
/// ([`lint_model_type_by_dir`]) both resolve and disagree — `staging/fct_x`
/// names a marts model filed under staging/. Either stage coming back
/// unresolved is NOT a violation (there's nothing to disagree with).
pub(super) fn model_directories(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let (Some(prefix_layer), Some(dir_layer)) = (
            lint_model_type_by_prefix(n, config),
            lint_model_type_by_dir(n, config),
        ) else {
            continue;
        };
        if prefix_layer == dir_layer {
            continue;
        }
        out.push(Finding::new(
            LintRule::ModelDirectories,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{} is named like a '{prefix_layer}' model but lives in the '{dir_layer}' directory",
                n.name
            ),
        ));
    }
}
