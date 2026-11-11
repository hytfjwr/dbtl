use super::*;

// ---- the rules -----------------------------------------------------------

/// Evaluate the enabled rules over `dag` and return every [`Finding`]:
/// pure and deterministic (rules in [`LintRule::ALL`] order, nodes scanned in
/// sorted `unique_id` order, `related` lists sorted).
///
/// Only ROOT-PROJECT nodes can be flagged — under `--include-packages` the
/// installed-package nodes stay dependency context (a package node's
/// neighbours still count toward a finding's fanout/join predicates, but the
/// flagged `node_uid` is always an in-scope node).
pub fn run_rules(dag: &Dag, config: &LintConfig) -> Vec<Finding> {
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
                    out.push(Finding {
                        rule,
                        node_uid: child,
                        related: vec![parent],
                        message: format!("{cname} directly depends on {pname} (a later layer)"),
                    });
                }
            }
        }
    }
    out
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
        out.push(Finding {
            rule: LintRule::DirectJoinToSource,
            node_uid: (*uid).clone(),
            related,
            message: format!(
                "{} directly depends on {n_sources} source(s) and {n_models} model(s)",
                n.name
            ),
        });
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
        out.push(Finding {
            rule: LintRule::MultipleSourcesJoined,
            node_uid: (*uid).clone(),
            related: sources.clone(),
            message: format!("{} directly depends on {} sources", n.name, sources.len()),
        });
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
        if models.len() < config.source_fanout_threshold {
            continue;
        }
        models.sort();
        out.push(Finding {
            rule: LintRule::SourceFanout,
            node_uid: (*uid).clone(),
            related: models.clone(),
            message: format!("source {} directly feeds {} models", n.name, models.len()),
        });
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
        if leaves.len() < config.model_fanout_threshold {
            continue;
        }
        leaves.sort();
        out.push(Finding {
            rule: LintRule::ModelFanout,
            node_uid: (*uid).clone(),
            related: leaves.clone(),
            message: format!("{} directly feeds {} leaf models", n.name, leaves.len()),
        });
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
        out.push(Finding {
            rule: LintRule::RootModels,
            node_uid: (*uid).clone(),
            related: Vec::new(),
            message: format!("{} has no parents", n.name),
        });
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
        out.push(Finding {
            rule: LintRule::RejoiningUpstream,
            node_uid: (*uid).clone(),
            related,
            message: format!(
                "{} is a redundant hop: {anames} already feed(s) {cname} directly",
                n.name
            ),
        });
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
        out.push(Finding {
            rule: LintRule::MartsDependOnSource,
            node_uid: (*uid).clone(),
            related: sources.clone(),
            message: format!(
                "{layer} model {} directly depends on {} source(s)",
                n.name,
                sources.len()
            ),
        });
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
        out.push(Finding {
            rule: LintRule::MissingPrimaryKeyTests,
            node_uid: (*uid).clone(),
            related: Vec::new(),
            message: format!(
                "{} has no unique+not_null test pair (or unique_combination_of_columns)",
                n.name
            ),
        });
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
        out.push(Finding {
            rule: LintRule::PublicModelsWithoutContract,
            node_uid: (*uid).clone(),
            related: Vec::new(),
            message: format!("{} is access: public without an enforced contract", n.name),
        });
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
        out.push(Finding {
            rule: LintRule::UndocumentedModels,
            node_uid: (*uid).clone(),
            related: Vec::new(),
            message: format!("{} has no description", n.name),
        });
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
        out.push(Finding {
            rule: LintRule::NamingConventions,
            node_uid: (*uid).clone(),
            related: Vec::new(),
            message: format!(
                "{layer} model {} doesn't match prefix(es): {}",
                n.name,
                prefixes.join(", ")
            ),
        });
    }
}
