use std::collections::BTreeSet;

use super::*;
use crate::select::select;

// ---- the rules -----------------------------------------------------------

/// The inputs a lint evaluation reads. `dag` and `config` are the only ones
/// a rule needs today; richer inputs (a SQL resolver, a catalog) join the
/// struct as fields rather than widening [`run_rules`]' signature.
#[derive(Debug, Clone, Copy)]
pub struct LintInput<'a> {
    pub dag: &'a Dag,
    pub config: &'a LintConfig,
}

impl<'a> LintInput<'a> {
    pub fn new(dag: &'a Dag, config: &'a LintConfig) -> Self {
        LintInput { dag, config }
    }
}

/// Evaluate the enabled rules over the input's dag and return every
/// [`Finding`]: pure and deterministic (rules in [`LintRule::ALL`] order,
/// nodes scanned in sorted `unique_id` order, `related` lists sorted).
///
/// Only ROOT-PROJECT nodes can be flagged — under `--include-packages` the
/// installed-package nodes stay dependency context (a package node's
/// neighbours still count toward a finding's fanout/join predicates, but the
/// flagged `node_uid` is always an in-scope node).
pub fn run_rules(input: LintInput<'_>) -> Vec<Finding> {
    let LintInput { dag, config } = input;
    // Resolve the owning package ONCE — `is_root_project` re-resolves per call.
    let primary = dag.primary_package();
    let in_scope = |n: &NodeInfo| primary.as_deref().is_none_or(|p| in_package(n, p));
    let mut uids: Vec<&String> = dag.nodes().keys().collect();
    uids.sort();
    let mut out = Vec::new();
    for rule in LintRule::ALL {
        if !config.enabled(rule) {
            continue;
        }
        match rule {
            LintRule::DirectJoinToSource => direct_join_to_source(dag, &uids, &in_scope, &mut out),
            LintRule::MultipleSourcesJoined => {
                multiple_sources_joined(dag, &uids, &in_scope, &mut out)
            }
            LintRule::SourceFanout => source_fanout(dag, &uids, &in_scope, config, &mut out),
            LintRule::ModelFanout => model_fanout(dag, &uids, &in_scope, config, &mut out),
            LintRule::RootModels => root_models(dag, &uids, &in_scope, &mut out),
            LintRule::RejoiningUpstream => rejoining_upstream(dag, &uids, &in_scope, &mut out),
            LintRule::MartsDependOnSource => {
                marts_depend_on_source(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::MissingPrimaryKeyTests => {
                missing_primary_key_tests(dag, &uids, &in_scope, &mut out)
            }
            LintRule::PublicModelsWithoutContract => {
                public_models_without_contract(dag, &uids, &in_scope, &mut out)
            }
            LintRule::UndocumentedModels => undocumented_models(dag, &uids, &in_scope, &mut out),
            LintRule::NamingConventions => {
                naming_conventions(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::LayerViolation => {
                for (parent, child) in layer_violation_edges(dag) {
                    let pname = dag.get(&parent).map_or(&*parent, |n| &n.name).to_string();
                    let cname = dag.get(&child).map_or(&*child, |n| &n.name).to_string();
                    out.push(Finding::new(
                        rule,
                        child,
                        vec![parent],
                        format!("{cname} directly depends on {pname} (a later layer)"),
                    ));
                }
            }
        }
    }
    // The envelope fields are filled in ONE place — the severity the config
    // resolved for the rule, and the node's project file. Rule functions
    // produce bare findings (`Finding::new`) so the policy stays here.
    for f in &mut out {
        f.severity = config
            .severity(f.rule)
            .unwrap_or_else(|| f.rule.info().default_severity);
        f.location = dag
            .detail(&f.node_uid)
            .and_then(|d| d.original_file_path.clone())
            .map(|file| Location { file, line: None });
    }
    suppress(dag, config, &mut out);
    out
}

/// Drop findings suppressed by `lint.rules.<id>.exclude` selectors or by
/// node `meta: {dbtl: {ignore: ...}}`. Both match on the finding's
/// `node_uid` — the CHILD side for edge rules — and remove findings whole,
/// never reordering them.
fn suppress(dag: &Dag, config: &LintConfig, out: &mut Vec<Finding>) {
    // Each rule's exclude spec evaluates ONCE into a uid set (the strings
    // are a union — `exclude: [a, b]` drops nodes matching a OR b).
    let excluded: BTreeMap<LintRule, BTreeSet<String>> = LintRule::ALL
        .iter()
        .filter_map(|rule| {
            let specs = config.excludes(*rule);
            if specs.is_empty() {
                None
            } else {
                let mut set = BTreeSet::new();
                for spec in specs {
                    set.extend(select(dag, None, spec));
                }
                Some((*rule, set))
            }
        })
        .collect();
    out.retain(|f| {
        !excluded
            .get(&f.rule)
            .is_some_and(|s| s.contains(&f.node_uid))
            && !meta_ignores(dag, &f.node_uid, f.rule)
    });
}

/// Does node `node_uid`'s `meta.dbtl.ignore` cover `rule`? The `dbtl` entry
/// is a JSON object (stringified by the `Dag`'s meta normalization):
/// `{"ignore": ["rule_id", ...]}` suppresses the listed rules, `{"ignore":
/// "all"}` suppresses every rule. A malformed value suppresses nothing —
/// suppression is opt-in, never accidental — and unknown rule ids in the
/// list are ignored (a stale id is not an error).
fn meta_ignores(dag: &Dag, node_uid: &str, rule: LintRule) -> bool {
    let Some(raw) = dag.detail(node_uid).and_then(|d| d.meta.get("dbtl")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    match v.get("ignore") {
        Some(serde_json::Value::String(s)) => s == "all",
        Some(serde_json::Value::Array(ids)) => ids
            .iter()
            .any(|id| id.as_str().is_some_and(|s| s == rule.name())),
        _ => false,
    }
}

/// Model parents + source parents of `uid`, split. Neighbours are counted
/// regardless of package (they're the flagged node's real dependency shape);
/// the FLAG itself is scoped by the caller.
pub(super) fn model_and_source_parents(dag: &Dag, uid: &str) -> (Vec<String>, Vec<String>) {
    let mut models = Vec::new();
    let mut sources = Vec::new();
    for p in dag.direct_parents(uid) {
        match dag.get(p) {
            Some(n) if n.resource_type == "model" => models.push(p.clone()),
            Some(n) if n.resource_type == "source" => sources.push(p.clone()),
            _ => {}
        }
    }
    models.sort();
    sources.sort();
    (models, sources)
}

/// `direct_join_to_source`: a model directly depends on both a source and a
/// model.
pub(super) fn direct_join_to_source(
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
        let (models, sources) = model_and_source_parents(dag, uid);
        if models.is_empty() || sources.is_empty() {
            continue;
        }
        let n_sources = sources.len();
        let n_models = models.len();
        let mut related = sources;
        related.extend(models);
        out.push(Finding::new(
            LintRule::DirectJoinToSource,
            (*uid).clone(),
            related,
            format!(
                "{} directly depends on {n_sources} source(s) and {n_models} model(s)",
                n.name
            ),
        ));
    }
}

/// `multiple_sources_joined`: a model directly depends on 2+ sources.
pub(super) fn multiple_sources_joined(
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
        let (_, sources) = model_and_source_parents(dag, uid);
        if sources.len() < 2 {
            continue;
        }
        out.push(Finding::new(
            LintRule::MultipleSourcesJoined,
            (*uid).clone(),
            sources.clone(),
            format!("{} directly depends on {} sources", n.name, sources.len()),
        ));
    }
}

/// `source_fanout`: a source directly feeds >= `source_fanout_threshold`
/// models (default 2 — the evaluator's ">1").
pub(super) fn source_fanout(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let threshold = config
        .int_param(LintRule::SourceFanout, "threshold")
        .expect("the registry declares source_fanout.threshold");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "source" || !in_scope(n) {
            continue;
        }
        let mut models: Vec<String> = dag
            .direct_children(uid)
            .iter()
            .filter(|c| dag.get(c).is_some_and(|n| n.resource_type == "model"))
            .cloned()
            .collect();
        if models.len() < threshold {
            continue;
        }
        models.sort();
        out.push(Finding::new(
            LintRule::SourceFanout,
            (*uid).clone(),
            models.clone(),
            format!("source {} directly feeds {} models", n.name, models.len()),
        ));
    }
}

/// `model_fanout`: a model directly feeds >= `model_fanout_threshold` leaf
/// models (models with no children of their own — the same `direct_down == 0`
/// leaf notion the orphan worklist uses).
pub(super) fn model_fanout(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let threshold = config
        .int_param(LintRule::ModelFanout, "threshold")
        .expect("the registry declares model_fanout.threshold");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let mut leaves: Vec<String> = dag
            .direct_children(uid)
            .iter()
            .filter(|c| {
                dag.get(c)
                    .is_some_and(|n| n.resource_type == "model" && n.direct_down == 0)
            })
            .cloned()
            .collect();
        if leaves.len() < threshold {
            continue;
        }
        leaves.sort();
        out.push(Finding::new(
            LintRule::ModelFanout,
            (*uid).clone(),
            leaves.clone(),
            format!("{} directly feeds {} leaf models", n.name, leaves.len()),
        ));
    }
}

/// `root_models`: a model with no parents.
pub(super) fn root_models(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) || n.direct_up != 0 {
            continue;
        }
        out.push(Finding::new(
            LintRule::RootModels,
            (*uid).clone(),
            Vec::new(),
            format!("{} has no parents", n.name),
        ));
    }
}

/// The resource types that count as "used" for [`rejoining_upstream`]'s
/// child test and as endpoints of the rejoin — the evaluator's
/// `single_use_resources` notion: data-carrying nodes only. Endpoint
/// resources (exposures, metrics, semantic models, saved queries) ride the
/// same `direct_children`/`direct_parents` lists but are NOT a "use" — a
/// model consumed only by an exposure is still single-use.
pub(super) fn is_data_resource(resource_type: &str) -> bool {
    matches!(resource_type, "model" | "seed" | "snapshot" | "source")
}

/// `rejoining_upstream`: node B whose ONLY data-node child is C, while some
/// data-node parent A of B also feeds C directly (A→B→C plus A→C — B is a
/// redundant hop). Matches the evaluator's `single_use_resources`: the child
/// count ignores endpoint resources, and A/C aren't model-only (a source→B→C
/// + source→C or a snapshot relay rejoin counts too).
///
/// One finding per B — `related` is the sorted set `{C} ∪ {A₁, A₂, …}` and
/// `message` names the offenders, so a B bypassed by several parents shows
/// once in worklists.
pub(super) fn rejoining_upstream(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_data_resource(&n.resource_type) || !in_scope(n) {
            continue;
        }
        // "Otherwise unused": exactly ONE data-node child. Endpoint resources
        // (an exposure reading B, say) don't count as a second use.
        let mut children = dag.direct_children(uid).iter().filter(|c| {
            dag.get(c)
                .is_some_and(|c| is_data_resource(&c.resource_type))
        });
        let (Some(c_uid), None) = (children.next(), children.next()) else {
            continue;
        };
        let c_uid = c_uid.clone();
        // Every data-node parent of B that ALSO feeds C directly. A/C carry
        // no scope check — under --include-packages a package neighbour is
        // dependency context, never the flagged node (B always is).
        let mut a_uids: Vec<String> = dag
            .direct_parents(uid)
            .iter()
            .filter(|a| {
                dag.get(a)
                    .is_some_and(|a| is_data_resource(&a.resource_type))
            })
            .filter(|a| dag.direct_parents(&c_uid).contains(*a))
            .cloned()
            .collect();
        if a_uids.is_empty() {
            continue;
        }
        a_uids.sort();
        a_uids.dedup();
        let cname = dag
            .get(&c_uid)
            .map_or_else(|| c_uid.clone(), |c| c.name.clone());
        let anames = a_uids
            .iter()
            .map(|a| dag.get(a).map_or(a.as_str(), |a| a.name.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut related = vec![c_uid];
        related.extend(a_uids.iter().cloned());
        related.sort();
        related.dedup();
        out.push(Finding::new(
            LintRule::RejoiningUpstream,
            (*uid).clone(),
            related,
            format!(
                "{} is a redundant hop: {anames} already feed(s) {cname} directly",
                n.name
            ),
        ));
    }
}

/// Resolve a model's lint layer the way dbt_project_evaluator resolves
/// `model_type` — **prefix first**, then folder, else `"other"`:
///
/// 1. a [`naming_prefixes`](LintConfig::naming_prefixes) layer whose
///    configured prefix the model name starts with (prefix-derived type);
/// 2. the [`first_dir`] folder, when it is itself a configured layer
///    (folder-derived type — an unmapped dir contributes nothing);
/// 3. `"other"`, the evaluator's catch-all.
///
/// Returning the CONFIG KEY (not the dir) keeps the result comparable across
/// consumers: `marts_depend_on_source` matches it against `marts` /
/// `intermediate`, and `naming_conventions` looks the expected prefixes back
/// up in the same map. The `"other"` type only carries prefixes when the
/// config defines an `other:` layer (evaluator-parity `rpt_` enforcement is
/// opt-in — see the README's Differences section).
pub(super) fn lint_model_type<'a>(n: &NodeInfo, config: &'a LintConfig) -> &'a str {
    // Prefix-derived type: BTreeMap order makes a name matching two layers'
    // prefixes resolve to the alphabetically-first layer — deterministic.
    for (layer, prefixes) in &config.naming_prefixes {
        if prefixes.iter().any(|p| n.name.starts_with(p.as_str())) {
            return layer.as_str();
        }
    }
    // Folder-derived type: only when the first dir is a configured layer (an
    // unmapped dir is not a type — the evaluator maps it to 'other', not to
    // an implicit layer).
    if let Some(dir) = first_dir(n) {
        if let Some((layer, _)) = config.naming_prefixes.get_key_value(dir) {
            return layer.as_str();
        }
    }
    "other"
}

/// `marts_depend_on_source`: a marts/intermediate model directly depends on a
/// source (raw-source reads belong in staging). Layer resolution is
/// [`lint_model_type`] (prefix-first like the evaluator — `staging/fct_x` is
/// a marts model, `reports/x` is 'other'). Any marts-layer model with a
/// source parent fires — unlike `direct_join_to_source`, no model parent is
/// required.
pub(super) fn marts_depend_on_source(
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
        if !matches!(layer, "marts" | "intermediate") {
            continue;
        }
        let (_, sources) = model_and_source_parents(dag, uid);
        if sources.is_empty() {
            continue;
        }
        out.push(Finding::new(
            LintRule::MartsDependOnSource,
            (*uid).clone(),
            sources.clone(),
            format!(
                "{layer} model {} directly depends on {} source(s)",
                n.name,
                sources.len()
            ),
        ));
    }
}

/// `missing_primary_key_tests`: a model lacking `unique` + `not_null` on the
/// same column AND lacking a `unique_combination_of_columns` test.
pub(super) fn missing_primary_key_tests(
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

/// `naming_conventions`: a model whose [`lint_model_type`] is listed in
/// `naming_prefixes` must start with one of that type's prefixes. The type is
/// resolved prefix-first like the evaluator — `staging/fct_x` is a *marts*
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
