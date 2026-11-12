//! `modeling` — DAG shape and layering rules: join mixes, fanout, roots,
//! redundant hops, layer direction, source hygiene and join hubs.

use super::*;

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
/// models (default 2 — more than one direct consumer).
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

/// `rejoining_upstream`: node B whose ONLY data-node child is C, while some
/// data-node parent A of B also feeds C directly (A→B→C plus A→C — B is a
/// redundant hop). "Single use" counts data-carrying children only: the
/// child count ignores endpoint resources, and A/C aren't model-only (a
/// source→B→C + source→C or a snapshot relay rejoin counts too).
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

/// `marts_depend_on_source`: a marts/intermediate model directly depends on a
/// source (raw-source reads belong in staging). Layer resolution is
/// [`lint_model_type`] (prefix first, then folder — `staging/fct_x` is
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

/// `layer_violation`: a backward edge between logical layers — the shared
/// [`layer_violation_edges`] predicate, surfaced as a lint rule. `node_uid`
/// is the CHILD endpoint (the model doing the backward dependency).
pub(super) fn layer_violation(dag: &Dag, out: &mut Vec<Finding>) {
    for (parent, child) in layer_violation_edges(dag) {
        let pname = dag.get(&parent).map_or(&*parent, |n| &n.name).to_string();
        let cname = dag.get(&child).map_or(&*child, |n| &n.name).to_string();
        out.push(Finding::new(
            LintRule::LayerViolation,
            child,
            vec![parent],
            format!("{cname} directly depends on {pname} (a later layer)"),
        ));
    }
}

/// `unused_sources`: a source no node reads — an exposure child counts as a
/// use like any other (`direct_down` covers it).
pub(super) fn unused_sources(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "source" || !in_scope(n) || n.direct_down != 0 {
            continue;
        }
        out.push(Finding::new(
            LintRule::UnusedSources,
            (*uid).clone(),
            Vec::new(),
            format!("source {} is not referenced by any node", n.name),
        ));
    }
}

/// `duplicate_sources`: two or more sources resolving to the same relation —
/// `database.schema.identifier` compared case-insensitively (identifier =
/// the source's `relation_name` when it differs from `name`, else `name`).
/// A schema either side lacks disqualifies the pair (never flag — source
/// mode may not have parsed it); a database either side lacks is simply not
/// compared. One finding per source, `related` = the other sources hitting
/// the same relation (uid order).
pub(super) fn duplicate_sources(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    // `(identifier, schema, database)` — the relation coordinates a source
    // resolves to.
    let coords = |uid: &str| -> Option<(String, Option<String>, Option<String>)> {
        let n = dag.get(uid)?;
        let d = dag.detail(uid)?;
        Some((
            d.relation_name.clone().unwrap_or_else(|| n.name.clone()),
            d.schema.clone(),
            d.database.clone(),
        ))
    };
    let same_relation = |a: &(String, Option<String>, Option<String>),
                         b: &(String, Option<String>, Option<String>)| {
        a.0.eq_ignore_ascii_case(&b.0)
            && match (&a.1, &b.1) {
                // Compared only when BOTH carry it — a missing schema means
                // "unknown", never "different".
                (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
                _ => false,
            }
            && match (&a.2, &b.2) {
                // Compared only when both carry it; a missing one is skipped,
                // not disqualifying (dbt fills it in manifest mode anyway).
                (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
                _ => true,
            }
    };
    // Pairwise, not key-partitioned: a missing database acts as a wildcard,
    // so "same relation" isn't transitive — each source lists its own
    // matches (uid order, since `uids` is already sorted).
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "source" || !in_scope(n) {
            continue;
        }
        let Some(mine) = coords(uid) else { continue };
        let mut related: Vec<String> = Vec::new();
        for other in uids {
            if other == uid {
                continue;
            }
            let Some(o) = dag.get(other) else { continue };
            if o.resource_type != "source" {
                continue;
            }
            if coords(other).is_some_and(|theirs| same_relation(&mine, &theirs)) {
                related.push((*other).clone());
            }
        }
        if related.is_empty() {
            continue;
        }
        let names = related
            .iter()
            .map(|u| dag.get(u).map_or(u.as_str(), |n| n.name.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Finding::new(
            LintRule::DuplicateSources,
            (**uid).clone(),
            related,
            format!("source {} points at the same relation as {names}", n.name),
        ));
    }
}

/// `staging_depends_on_staging`: a staging-layer model directly depends on
/// another staging-layer model. Layer resolution is [`lint_model_type`]
/// (prefix first — the same resolution every layer-aware
/// rule shares).
pub(super) fn staging_depends_on_staging(
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
        if lint_model_type(n, config) != "staging" {
            continue;
        }
        let mut parents: Vec<String> = dag
            .direct_parents(uid)
            .iter()
            .filter(|p| {
                dag.get(p).is_some_and(|p| {
                    p.resource_type == "model" && lint_model_type(p, config) == "staging"
                })
            })
            .cloned()
            .collect();
        if parents.is_empty() {
            continue;
        }
        parents.sort();
        out.push(Finding::new(
            LintRule::StagingDependsOnStaging,
            (*uid).clone(),
            parents.clone(),
            format!(
                "staging model {} directly depends on {} staging model(s)",
                n.name,
                parents.len()
            ),
        ));
    }
}

/// `too_many_joins`: a model directly depends on >= `threshold` (default 7)
/// data-resource parents ([`is_data_resource`] — endpoint resources like
/// exposures never parent a model anyway, but the predicate is the shared
/// one).
pub(super) fn too_many_joins(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let threshold = config
        .int_param(LintRule::TooManyJoins, "threshold")
        .expect("the registry declares too_many_joins.threshold");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let mut parents: Vec<String> = dag
            .direct_parents(uid)
            .iter()
            .filter(|p| {
                dag.get(p)
                    .is_some_and(|p| is_data_resource(&p.resource_type))
            })
            .cloned()
            .collect();
        if parents.len() < threshold {
            continue;
        }
        parents.sort();
        out.push(Finding::new(
            LintRule::TooManyJoins,
            (*uid).clone(),
            parents.clone(),
            format!(
                "{} directly depends on {} data resources",
                n.name,
                parents.len()
            ),
        ));
    }
}

/// `incremental_without_unique_key`: a model materialized `incremental`
/// whose effective `unique_key` is empty (the normalized `NodeDetail` list —
/// in-file `config()`, schema.yml `config:` and the project tree all land
/// there already, so the rule reads one field).
pub(super) fn incremental_without_unique_key(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model"
            || n.materialized.as_deref() != Some("incremental")
            || !in_scope(n)
        {
            continue;
        }
        if dag.detail(uid).is_some_and(|d| !d.unique_key.is_empty()) {
            continue;
        }
        out.push(Finding::new(
            LintRule::IncrementalWithoutUniqueKey,
            (*uid).clone(),
            Vec::new(),
            format!("incremental model {} has no unique_key", n.name),
        ));
    }
}
