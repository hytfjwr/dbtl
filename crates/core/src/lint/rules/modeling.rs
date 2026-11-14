//! `modeling` — DAG shape and layering rules: join mixes, fanout, roots,
//! redundant hops, layer direction, source hygiene and join hubs.

use super::*;

/// Exactly one OTHER staging-layer model reads `source` directly → the
/// concrete model to `ref` instead. More or fewer readers → no confident
/// fix suggestion.
fn single_staging_reader(
    dag: &Dag,
    source: &str,
    excluding: &str,
    config: &LintConfig,
) -> Option<String> {
    let mut readers: Vec<&String> = dag
        .direct_children(source)
        .iter()
        .filter(|c| c.as_str() != excluding)
        .filter(|c| {
            dag.get(c).is_some_and(|n| {
                n.resource_type == "model" && lint_model_type(n, config) == "staging"
            })
        })
        .collect();
    readers.sort();
    (readers.len() == 1).then(|| display_name(dag, readers[0]))
}

/// `direct_join_to_source`: a model directly depends on both a source and a
/// model. The position lands on the first `source()` call — `source()`
/// labels mark the raw reads, `ref()` labels the model reads.
pub(super) fn direct_join_to_source(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    sites: &CallSiteResolver<'_>,
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
        let mut related = sources.clone();
        related.extend(models.iter().cloned());
        let mut f = Finding::new(
            LintRule::DirectJoinToSource,
            (*uid).clone(),
            related,
            format!(
                "{} reads {} {} and {} {}",
                display_name(dag, uid),
                pluralize(sources.len(), "source"),
                name_list(&display_names(dag, &sources)),
                pluralize(models.len(), "model"),
                name_list(&display_names(dag, &models)),
            ),
        );
        if sources.len() == 1 {
            if let Some(stg) = single_staging_reader(dag, &sources[0], uid, config) {
                f = f.with_help(format!("select from {stg} instead"));
            }
        }
        if let Some(cs) = sites.for_node(uid) {
            let mut src_spans: Vec<_> = sources.iter().flat_map(|s| cs.calls_to(s)).collect();
            src_spans.sort_by_key(|r| r.start);
            if let Some(first) = src_spans.first().cloned() {
                let secondary = src_spans[1..]
                    .iter()
                    .map(|r| (r.clone(), "source".to_string()))
                    .chain(
                        models
                            .iter()
                            .flat_map(|m| cs.calls_to(m))
                            .map(|r| (r, "model".to_string())),
                    )
                    .collect();
                f = cs.annotate(f, (first, "source"), secondary);
            }
        }
        out.push(f);
    }
}

/// `multiple_sources_joined`: a model directly depends on 2+ sources. The
/// position lands on the first `source()` call; the rest are labels.
pub(super) fn multiple_sources_joined(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sites: &CallSiteResolver<'_>,
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
        // `{% if %}`/`{% elif %}`/`{% else %}`/`{% for %}` arms are
        // mutually exclusive — dbt renders exactly one per chain, so
        // sources reachable only through exclusive arms never appear in
        // one render and count once. The graph still carries both
        // edges (lineage unchanged); only the "simultaneously read"
        // claim needs the arm structure — a call site it can't
        // attribute counts unconditional, the under-reporting side.
        let call_sites = sites.for_node(uid);
        let simultaneous = call_sites
            .as_ref()
            .map(|cs| cs.simultaneous_parents(&sources))
            .unwrap_or(sources.len());
        if simultaneous < 2 {
            continue;
        }
        let mut f = Finding::new(
            LintRule::MultipleSourcesJoined,
            (*uid).clone(),
            sources.clone(),
            format!(
                "{} reads {} {}",
                display_name(dag, uid),
                pluralize(sources.len(), "source"),
                name_list(&display_names(dag, &sources)),
            ),
        );
        if let Some(cs) = call_sites {
            let mut spans: Vec<_> = sources.iter().flat_map(|s| cs.calls_to(s)).collect();
            spans.sort_by_key(|r| r.start);
            if let Some((first, rest)) = spans.split_first() {
                f = cs.annotate(
                    f,
                    (first.clone(), "source"),
                    rest.iter()
                        .map(|r| (r.clone(), "source".to_string()))
                        .collect(),
                );
            }
        }
        out.push(f);
    }
}

/// `source_fanout`: a source directly feeds >= `source_fanout_threshold`
/// models (default 2 — more than one direct consumer). Versions of one
/// model are the SAME consumer — `fct_orders.v1`+`v2`+`vbeta` reading the
/// source counts once ([`version_families`] folds by `(package, name)`).
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
        models.sort();
        let families = version_families(&models);
        if families.len() < threshold {
            continue;
        }
        let names: Vec<String> = families.iter().map(|f| family_name(dag, f)).collect();
        out.push(Finding::new(
            LintRule::SourceFanout,
            (*uid).clone(),
            models.clone(),
            format!(
                "source {} directly feeds {} {}: {}",
                display_name(dag, uid),
                families.len(),
                pluralize(families.len(), "model"),
                name_list(&names),
            ),
        ));
    }
}

/// `model_fanout`: a model directly feeds >= `model_fanout_threshold` leaf
/// models. A leaf is a model with no DATA-RESOURCE children of its own —
/// a model feeding only endpoint resources (an exposure, a semantic
/// model, a metric, a saved query) is still a leaf, matching how dbt's
/// fanout check reads the graph — PLUS no package-severed children: a
/// child only an installed package reads can't prove leaf-ness (the
/// [`PrunedAdjacent`](crate::PrunedAdjacent) record), so it never counts.
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
                dag.get(c).is_some_and(|n| {
                    n.resource_type == "model"
                        && dag.direct_children(c).iter().all(|gc| {
                            dag.get(gc)
                                .is_none_or(|g| !is_data_resource(&g.resource_type))
                        })
                        && dag.pruned_adjacent(c).is_none_or(|a| a.children == 0)
                })
            })
            .cloned()
            .collect();
        leaves.sort();
        // Version spellings of one model are the same leaf — a hub
        // feeding `x.v1`+`x.v2` feeds one consumer, not two.
        let families = version_families(&leaves);
        if families.len() < threshold {
            continue;
        }
        let names: Vec<String> = families.iter().map(|f| family_name(dag, f)).collect();
        out.push(Finding::new(
            LintRule::ModelFanout,
            (*uid).clone(),
            leaves.clone(),
            format!(
                "{} directly feeds {} {}: {}",
                display_name(dag, uid),
                families.len(),
                pluralize(families.len(), "leaf model"),
                name_list(&names),
            ),
        ));
    }
}

/// `root_models`: a model with no parents. Two connectedness shapes the
/// pruned graph can't see don't count: parents a `PackageScope::Root`
/// prune severed (the [`PrunedAdjacent`](crate::PrunedAdjacent) record —
/// the model IS connected, just outside the project), and dependency
/// calls whose targets never resolved to an edge (source mode parses no
/// installed packages, so a cross-package `ref()` drops silently — an
/// under-resolved model, not a root).
pub(super) fn root_models(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sites: &CallSiteResolver<'_>,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) || n.direct_up != 0 {
            continue;
        }
        if dag.pruned_adjacent(uid).is_some_and(|a| a.parents > 0) {
            continue;
        }
        if sites
            .for_node(uid)
            .is_some_and(|cs| cs.has_dependency_calls())
        {
            continue;
        }
        out.push(Finding::new(
            LintRule::RootModels,
            (*uid).clone(),
            Vec::new(),
            format!("{} has no parents", display_name(dag, uid)),
        ));
    }
}

/// `rejoining_upstream`: node B whose ONLY data-node child is C, while some
/// data-node parent A of B also feeds C directly (A→B→C plus A→C — B is a
/// redundant hop). "Single use" counts data-carrying children only: the
/// child count ignores endpoint resources, and A/C aren't model-only (a
/// source→B→C + source→C or a snapshot relay rejoin counts too). A B whose
/// package-side children a `PackageScope::Root` prune severed (the
/// [`PrunedAdjacent`](crate::PrunedAdjacent) record) can never ESTABLISH
/// single use — the missing consumer's resource type is invisible — so it
/// never flags.
///
/// One finding per B — `related` is the sorted set `{C} ∪ {A₁, A₂, …}` and
/// `message` names the offenders, so a B bypassed by several parents shows
/// once in worklists. When B is also read by endpoint consumers (an
/// exposure, a metric, a `semantic_models:` entry), the message names
/// them — the verdict still holds because endpoints don't carry data
/// downstream, but a naked "redundant hop" would read as "nothing reads B".
///
/// `ignore_shared_parents` (selectors, default empty) exempts Bs whose
/// shared parents ALL match — the dimensional shape where a fact's models
/// join the same `dim_*` their shared dimension already provides
/// (`["dim_*"]`). Exempting by A can't ride `exclude:` — that filters the
/// flagged node B, never its parent.
pub(super) fn rejoining_upstream(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    sites: &CallSiteResolver<'_>,
    out: &mut Vec<Finding>,
    notes: &mut BTreeSet<String>,
) {
    // Resolve the ignore set once — the specs were selector-validated at
    // config load, so each evaluates without error paths. A spec written
    // in the legacy `path:` form notes a deprecation for the outcome.
    let mut ignored: BTreeSet<String> = BTreeSet::new();
    for spec in config
        .str_list_param(LintRule::RejoiningUpstream, "ignore_shared_parents")
        .expect("the registry declares rejoining_upstream.ignore_shared_parents")
    {
        let (set, spec_notes) = crate::select::select_report(dag, None, &spec);
        ignored.extend(set);
        notes.extend(spec_notes);
    }
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_data_resource(&n.resource_type) || !in_scope(n) {
            continue;
        }
        // "Otherwise unused": exactly ONE data-node child. Endpoint resources
        // (an exposure reading B, say) don't count as a second use — but a
        // pruned package child might, so its presence ends the check here.
        if dag.pruned_adjacent(uid).is_some_and(|a| a.children > 0) {
            continue;
        }
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
            // `ignore_shared_parents` exempts shared parents matching the
            // selectors — every A ignored means no bypass left to flag.
            .filter(|a| !ignored.contains(*a))
            .cloned()
            .collect();
        if a_uids.is_empty() {
            continue;
        }
        a_uids.sort();
        a_uids.dedup();
        // B's endpoint consumers — they read B but aren't "uses" (exposures,
        // metrics, saved queries, and `semantic_models:` entries; the
        // `semantic_dependents` channel names the last in BOTH modes —
        // manifest mode also carries them as child nodes, so dedupe on the
        // label). Name them so "redundant hop" doesn't read as "nothing
        // reads B at all".
        let mut consumers: BTreeSet<String> = dag
            .direct_children(uid)
            .iter()
            .filter(|c| {
                dag.get(c)
                    .is_some_and(|c| !is_data_resource(&c.resource_type))
            })
            .map(|c| {
                let cn = dag.get(c).expect("filtered above");
                format!("{} {}", cn.resource_type, display_name(dag, c))
            })
            .collect();
        if let Some(d) = dag.detail(uid) {
            consumers.extend(
                d.semantic_dependents
                    .iter()
                    .map(|n| format!("semantic_model {n}")),
            );
        }
        let note = if consumers.is_empty() {
            String::new()
        } else {
            format!(
                " (also read by {}; endpoint consumers do not count as uses)",
                name_list(&consumers.into_iter().collect::<Vec<_>>())
            )
        };
        let cname = display_name(dag, &c_uid);
        let anames = name_list(&display_names(dag, &a_uids));
        let mut related = vec![c_uid];
        related.extend(a_uids.iter().cloned());
        related.sort();
        related.dedup();
        let mut f = Finding::new(
            LintRule::RejoiningUpstream,
            (*uid).clone(),
            related,
            format!(
                "{} is a redundant hop: {anames} already {} {cname} directly{note}",
                display_name(dag, uid),
                if a_uids.len() == 1 { "feeds" } else { "feed" },
            ),
        );
        // The position is the ref in B that reads A directly — the edge C
        // makes redundant.
        if let Some(cs) = sites.for_node(uid) {
            let mut spans: Vec<_> = a_uids.iter().flat_map(|a| cs.calls_to(a)).collect();
            spans.sort_by_key(|r| r.start);
            if let Some(first) = spans.first().cloned() {
                let label = format!("also read directly by {cname}");
                f = cs.annotate(
                    f,
                    (first, label.clone()),
                    spans[1..]
                        .iter()
                        .map(|r| (r.clone(), label.clone()))
                        .collect(),
                );
            }
        }
        out.push(f);
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
    sites: &CallSiteResolver<'_>,
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
        let mut f = Finding::new(
            LintRule::MartsDependOnSource,
            (*uid).clone(),
            sources.clone(),
            format!(
                "{layer} model {} reads {} {}",
                display_name(dag, uid),
                pluralize(sources.len(), "source"),
                name_list(&display_names(dag, &sources)),
            ),
        );
        if sources.len() == 1 {
            if let Some(stg) = single_staging_reader(dag, &sources[0], uid, config) {
                f = f.with_help(format!("select from {stg} instead"));
            }
        }
        if let Some(cs) = sites.for_node(uid) {
            let mut spans: Vec<_> = sources.iter().flat_map(|s| cs.calls_to(s)).collect();
            spans.sort_by_key(|r| r.start);
            if let Some((first, rest)) = spans.split_first() {
                f = cs.annotate(
                    f,
                    (first.clone(), "source used here"),
                    rest.iter()
                        .map(|r| (r.clone(), "source used here".to_string()))
                        .collect(),
                );
            }
        }
        out.push(f);
    }
}

/// `layer_violation`: a backward edge between logical layers — the shared
/// [`layer_violation_edges`] predicate, surfaced as a lint rule. `node_uid`
/// is the CHILD endpoint (the model doing the backward dependency); the
/// position lands on the child's ref to the later-layer parent.
pub(super) fn layer_violation(dag: &Dag, sites: &CallSiteResolver<'_>, out: &mut Vec<Finding>) {
    for (parent, child) in layer_violation_edges(dag) {
        let pname = display_name(dag, &parent);
        let cname = display_name(dag, &child);
        let mut f = Finding::new(
            LintRule::LayerViolation,
            child.clone(),
            vec![parent.clone()],
            format!("{cname} directly depends on {pname} (a later layer)"),
        );
        if let Some(cs) = sites.for_node(&child) {
            let spans = cs.calls_to(&parent);
            if let Some(first) = spans.first().cloned() {
                let label = format!("{pname} is in a later layer");
                f = cs.annotate(
                    f,
                    (first, label.clone()),
                    spans[1..]
                        .iter()
                        .map(|r| (r.clone(), label.clone()))
                        .collect(),
                );
            }
        }
        out.push(f);
    }
}

/// `unused_sources`: a source no node reads — an exposure child counts as a
/// use like any other (`direct_down` covers it), and so does a child the
/// `PackageScope::Root` prune severed (the [`PrunedAdjacent`](crate::PrunedAdjacent)
/// record: a package model reading it IS a use — just not a visible one).
pub(super) fn unused_sources(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "source"
            || !in_scope(n)
            || n.direct_down != 0
            || dag.pruned_adjacent(uid).is_some_and(|a| a.children > 0)
        {
            continue;
        }
        out.push(Finding::new(
            LintRule::UnusedSources,
            (*uid).clone(),
            Vec::new(),
            format!(
                "source {} is not referenced by any node",
                display_name(dag, uid)
            ),
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
        let names = name_list(&display_names(dag, &related));
        out.push(Finding::new(
            LintRule::DuplicateSources,
            (**uid).clone(),
            related,
            format!(
                "source {} points at the same relation as {names}",
                display_name(dag, uid)
            ),
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
    sites: &CallSiteResolver<'_>,
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
        let mut f = Finding::new(
            LintRule::StagingDependsOnStaging,
            (*uid).clone(),
            parents.clone(),
            format!(
                "staging model {} directly depends on {} {}",
                display_name(dag, uid),
                pluralize(parents.len(), "staging model"),
                name_list(&display_names(dag, &parents)),
            ),
        );
        // The position is the first ref to a staging parent — the read
        // that skips the source.
        if let Some(cs) = sites.for_node(uid) {
            let mut spans: Vec<_> = parents.iter().flat_map(|p| cs.calls_to(p)).collect();
            spans.sort_by_key(|r| r.start);
            if let Some((first, rest)) = spans.split_first() {
                f = cs.annotate(
                    f,
                    (first.clone(), "staging model"),
                    rest.iter()
                        .map(|r| (r.clone(), "staging model".to_string()))
                        .collect(),
                );
            }
        }
        out.push(f);
    }
}

/// `too_many_joins`: a model directly depends on >= `threshold` (default 7)
/// data-resource parents ([`is_data_resource`] — endpoint resources like
/// exposures never parent a model anyway, but the predicate is the shared
/// one). Versions of one parent count once — joining `dim.v1` AND `dim.v2`
/// is still reading `dim` ([`version_families`]).
pub(super) fn too_many_joins(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    sites: &CallSiteResolver<'_>,
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
        parents.sort();
        let families = version_families(&parents);
        if families.len() < threshold {
            continue;
        }
        let names: Vec<String> = families.iter().map(|f| family_name(dag, f)).collect();
        let mut f = Finding::new(
            LintRule::TooManyJoins,
            (*uid).clone(),
            parents.clone(),
            format!(
                "{} directly depends on {} {}: {}",
                display_name(dag, uid),
                families.len(),
                pluralize(families.len(), "data resource"),
                name_list(&names),
            ),
        );
        // Primary on the first ref/source call; every other parent call is
        // a label naming what it reads.
        if let Some(cs) = sites.for_node(uid) {
            let mut spots: Vec<(std::ops::Range<usize>, String)> = Vec::new();
            for parent in &parents {
                for span in cs.calls_to(parent) {
                    spots.push((span, format!("reads {}", display_name(dag, parent))));
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

/// `incremental_without_unique_key`: a model materialized `incremental`
/// whose effective `unique_key` is empty (the normalized `NodeDetail` list —
/// in-file `config()`, schema.yml `config:` and the project tree all land
/// there already, so the rule reads one field). Strategies that never
/// merge — `append`, `insert_overwrite`, `microbatch` — don't take a
/// `unique_key` in dbt, so they never flag; the configured strategy names
/// itself in the message when set.
pub(super) fn incremental_without_unique_key(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sites: &CallSiteResolver<'_>,
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
        let detail = dag.detail(uid);
        let strategy = detail.and_then(|d| d.incremental_strategy.as_deref());
        if strategy.is_some_and(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "append" | "insert_overwrite" | "microbatch"
            )
        }) {
            continue;
        }
        if detail.is_some_and(|d| !d.unique_key.is_empty()) {
            continue;
        }
        let mut f = Finding::new(
            LintRule::IncrementalWithoutUniqueKey,
            (*uid).clone(),
            Vec::new(),
            match strategy {
                Some(s) => format!(
                    "incremental model {} has no unique_key (incremental_strategy: {s})",
                    display_name(dag, uid)
                ),
                None => format!(
                    "incremental model {} has no unique_key",
                    display_name(dag, uid)
                ),
            },
        );
        // The position is the `config()` argument that declared
        // `materialized='incremental'` — the key the fix sits next to.
        if let Some(cs) = sites.for_node(uid) {
            if let Some((span, value)) = cs.materialized_arg() {
                if value == "incremental" {
                    f = cs.annotate(f, (span, format!("materialized '{value}'")), vec![]);
                }
            }
        }
        out.push(f);
    }
}

/// `unused_models`: a model in a configured layer (default staging +
/// intermediate — marts are the DAG's endpoints by design) that NOTHING
/// reads. A child of any kind counts as a use (`direct_down` covers node
/// and endpoint resources alike — an exposure or a semantic-layer node
/// reading it keeps it alive), and so does a `semantic_models:` consumer
/// the [`NodeDetail::semantic_dependents`] channel records (source mode
/// synthesizes no semantic-layer nodes, so the channel is how both modes
/// answer "the semantic layer reads this" identically). And a child the
/// `PackageScope::Root` prune severed counts too (the
/// [`PrunedAdjacent`](crate::PrunedAdjacent) record — the model IS used,
/// just outside the kept project).
pub(super) fn unused_models(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let layers = config
        .str_list_param(LintRule::UnusedModels, "layers")
        .expect("the registry declares unused_models.layers");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let layer = lint_model_type(n, config);
        if !layers.iter().any(|l| l.as_str() == layer) {
            continue;
        }
        if n.direct_down != 0
            || dag.pruned_adjacent(uid).is_some_and(|a| a.children > 0)
            || dag
                .detail(uid)
                .is_some_and(|d| !d.semantic_dependents.is_empty())
        {
            continue;
        }
        out.push(Finding::new(
            LintRule::UnusedModels,
            (*uid).clone(),
            Vec::new(),
            format!("{layer} model {} has no downstream", display_name(dag, uid)),
        ));
    }
}

/// `unresolved_references`: a literal `ref()`/`source()` call names nothing
/// the load could see. The judgment runs against [`Dag::ref_scope`] — the
/// PRE-PRUNE name universe (installed packages, declared sources, parked
/// `disabled` names, versioned families) both load modes record — so a
/// call pointing at a package node the `PackageScope::Root` prune hid
/// still counts as resolved, while a call whose name is provably absent
/// flags. A parked name gets its own "disabled" wording: it fails dbt's
/// parse just like a missing one, but the fix is re-enabling, not
/// declaring.
///
/// Skipped, never flagged: a call inside an `{% if %}`/`{% elif %}`/
/// `{% else %}` arm (a Jinja-switched reference may legitimately not
/// exist in the taken arm — `{% for %}` bodies still judge, the literal
/// ref runs every render); a `v=`/`version=`-pinned ref in source mode
/// (versioned nodes are never synthesized); a `ref('proj', 'x')` naming
/// a `projects:`-declared cross-project name; and any call whose name is
/// missing while declared packages aren't fully installed — those count
/// into ONE summary warning instead (the name may live in a package
/// that was never installed, so the verdict is unverifiable, not
/// absent). A nonliteral arg (`ref(var('x'))`, `v=var('ver')`) produces
/// no call at all — the extractor only yields names it could read.
///
/// One finding per distinct failing target per node; every site of the
/// same target labels it.
pub(super) fn unresolved_references(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sites: &CallSiteResolver<'_>,
    out: &mut Vec<Finding>,
    notes: &mut BTreeSet<String>,
) {
    let scope = dag.ref_scope();
    let root = dag.project_name();
    let mut unverified = 0usize;
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !in_scope(n) {
            continue;
        }
        let Some(cs) = sites.for_node(uid) else {
            continue;
        };
        // (message, spans, span-label) — keyed by message so the same
        // failing target at several sites becomes one finding.
        let mut keyed: Vec<(String, Vec<std::ops::Range<usize>>, &'static str)> = Vec::new();
        for call in cs.dep_calls() {
            if call.in_if_arm {
                continue;
            }
            let fail = match &call.kind {
                DepCallKind::Ref {
                    name,
                    package,
                    versioned,
                    version,
                } => {
                    // Unjudgeable calls first: a nonliteral version kwarg
                    // (`v=var('ver')` — the NAME stays judgeable, but a
                    // wrong literal pin can't be told from a right one),
                    // and a cross-project target.
                    if (*versioned && version.is_none())
                        || package
                            .as_deref()
                            .is_some_and(|p| scope.projects.contains(p))
                    {
                        None
                    } else {
                        let target = package
                            .as_deref()
                            .map_or_else(|| name.clone(), |p| format!("{p}.{name}"));
                        let pkg_match = |dpkg: &str| package.as_deref().is_none_or(|p| p == dpkg);
                        match scope.disabled_refs.get(name) {
                            Some((dpkg, kind)) if pkg_match(dpkg) => Some((
                                format!(
                                    "{} references disabled {kind} '{target}'",
                                    display_name(dag, uid)
                                ),
                                "disabled",
                            )),
                            _ => {
                                let name_resolved = match package.as_deref() {
                                    None => {
                                        scope.refs.contains_key(name)
                                            || scope.assumed.contains(name)
                                    }
                                    Some(p) => {
                                        scope.refs.get(name).is_some_and(|pkgs| pkgs.contains(p))
                                            || (root == Some(p) && scope.assumed.contains(name))
                                    }
                                };
                                // A version pin is only checkable when the
                                // load records version uids (manifest mode);
                                // under a mode that never does (source),
                                // the NAME was already judged and the pin
                                // passes unjudgeable.
                                let resolved = name_resolved
                                    && match (versioned, version, &scope.versions) {
                                        (true, Some(v), Some(vs)) => {
                                            match package.as_deref() {
                                                Some(p) => vs.contains(&(
                                                    p.to_string(),
                                                    name.clone(),
                                                    v.clone(),
                                                )),
                                                // A bare ref's package is
                                                // unknown — accept the
                                                // version in ANY package
                                                // (under-reports when a
                                                // local unversioned x
                                                // shadows a packaged
                                                // versioned one: dbt would
                                                // error, we pass — the
                                                // approximation's safe
                                                // direction).
                                                None => {
                                                    vs.iter().any(|(_, n, vv)| n == name && vv == v)
                                                }
                                            }
                                        }
                                        _ => true,
                                    };
                                if resolved {
                                    None
                                } else if scope.uninstalled_packages > 0 {
                                    unverified += 1;
                                    None
                                } else {
                                    let message = match (name_resolved, version) {
                                        (true, Some(v)) => format!(
                                            "{} references '{target}' version '{v}', which resolves to no node",
                                            display_name(dag, uid)
                                        ),
                                        _ => format!(
                                            "{} references '{target}', which resolves to no node",
                                            display_name(dag, uid)
                                        ),
                                    };
                                    Some((message, "unresolved"))
                                }
                            }
                        }
                    }
                }
                DepCallKind::Source { source, table } => {
                    let pair = (source.clone(), table.clone());
                    if scope.disabled_sources.contains(&pair) {
                        Some((
                            format!(
                                "{} references disabled source '{source}.{table}'",
                                display_name(dag, uid)
                            ),
                            "disabled",
                        ))
                    } else if scope.sources.contains(&pair) {
                        None
                    } else if scope.uninstalled_packages > 0 {
                        unverified += 1;
                        None
                    } else {
                        Some((
                            format!(
                                "{} references source '{source}.{table}', which is not declared",
                                display_name(dag, uid)
                            ),
                            "not declared",
                        ))
                    }
                }
            };
            if let Some((message, label)) = fail {
                match keyed.iter_mut().find(|(m, ..)| *m == message) {
                    Some((_, spans, _)) => spans.push(call.span.clone()),
                    None => keyed.push((message, vec![call.span.clone()], label)),
                }
            }
        }
        for (message, spans, label) in keyed {
            let f = Finding::new(
                LintRule::UnresolvedReferences,
                (*uid).clone(),
                Vec::new(),
                message,
            );
            out.push(
                cs.annotate(
                    f,
                    (spans[0].clone(), label),
                    spans[1..]
                        .iter()
                        .map(|s| (s.clone(), label.to_string()))
                        .collect(),
                ),
            );
        }
    }
    if unverified > 0 {
        notes.insert(format!(
            "{unverified} unresolved {} skipped — {} declared {} not installed (run `dbt deps` to verify)",
            pluralize(unverified, "reference"),
            scope.uninstalled_packages,
            pluralize(scope.uninstalled_packages, "package"),
        ));
    }
}
