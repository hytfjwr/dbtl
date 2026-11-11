//! The rule evaluators — one `pub(super)` function per rule, grouped into a
//! file per category so sibling rule groups extend their own file without
//! touching this one. `run_rules` below is the ONE evaluation entry point
//! `dbtl check --lint`, the TUI's Lint lens and `dbtl docs` all share; the
//! shared predicates (layer resolution, parent classification, the scope
//! test) also live here so no category file re-implements them.

use std::collections::BTreeSet;

use super::*;
use crate::select::select;

mod documentation;
mod governance;
mod modeling;
mod performance;
mod structure;
mod testing;

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
            LintRule::DirectJoinToSource => {
                modeling::direct_join_to_source(dag, &uids, &in_scope, &mut out)
            }
            LintRule::MultipleSourcesJoined => {
                modeling::multiple_sources_joined(dag, &uids, &in_scope, &mut out)
            }
            LintRule::SourceFanout => {
                modeling::source_fanout(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::ModelFanout => {
                modeling::model_fanout(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::RootModels => modeling::root_models(dag, &uids, &in_scope, &mut out),
            LintRule::RejoiningUpstream => {
                modeling::rejoining_upstream(dag, &uids, &in_scope, &mut out)
            }
            LintRule::MartsDependOnSource => {
                modeling::marts_depend_on_source(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::MissingPrimaryKeyTests => {
                testing::missing_primary_key_tests(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::PublicModelsWithoutContract => {
                governance::public_models_without_contract(dag, &uids, &in_scope, &mut out)
            }
            LintRule::UndocumentedModels => {
                documentation::undocumented_models(dag, &uids, &in_scope, &mut out)
            }
            LintRule::NamingConventions => {
                structure::naming_conventions(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::LayerViolation => modeling::layer_violation(dag, &mut out),
            LintRule::UnusedSources => modeling::unused_sources(dag, &uids, &in_scope, &mut out),
            LintRule::DuplicateSources => {
                modeling::duplicate_sources(dag, &uids, &in_scope, &mut out)
            }
            LintRule::StagingDependsOnStaging => {
                modeling::staging_depends_on_staging(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::TooManyJoins => {
                modeling::too_many_joins(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::ChainedViews => {
                performance::chained_views(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::ExposureParentsMaterializations => {
                performance::exposure_parents_materializations(dag, &uids, &in_scope, &mut out)
            }
            LintRule::ExposuresDependentOnPrivateModels => {
                governance::exposures_dependent_on_private_models(dag, &uids, &in_scope, &mut out)
            }
            LintRule::UndocumentedSources => {
                documentation::undocumented_sources(dag, &uids, &in_scope, &mut out)
            }
            LintRule::UndocumentedColumns => {
                documentation::undocumented_columns(dag, &uids, &in_scope, &mut out)
            }
            LintRule::UndocumentedPublicModels => {
                governance::undocumented_public_models(dag, &uids, &in_scope, &mut out)
            }
            LintRule::ModelDirectories => {
                structure::model_directories(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::RequiredMetaKeys => {
                governance::required_meta_keys(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::ContractColumnsWithoutType => {
                governance::contract_columns_without_type(dag, &uids, &in_scope, &mut out)
            }
            LintRule::IncrementalWithoutUniqueKey => {
                modeling::incremental_without_unique_key(dag, &uids, &in_scope, &mut out)
            }
            LintRule::SourcesWithoutFreshness => {
                testing::sources_without_freshness(dag, &uids, &in_scope, &mut out)
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
        // YAML-declared-attribute findings (see YAML_ATTRIBUTE_RULE_IDS)
        // point at the declaring .yml when the loaders recorded one —
        // a missing column description lives in schema.yml, not the
        // model's .sql. Graph/SQL-level findings keep the original file.
        f.location = dag
            .detail(&f.node_uid)
            .and_then(|d| {
                if crate::lint::YAML_ATTRIBUTE_RULE_IDS.contains(&f.rule.name()) {
                    d.patch_path
                        .clone()
                        .or_else(|| d.original_file_path.clone())
                } else {
                    d.original_file_path.clone()
                }
            })
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

/// The resource types that count as "used" for [`rejoining_upstream`]'s
/// child test and as endpoints of the rejoin — the evaluator's
/// `single_use_resources` notion: data-carrying nodes only. Endpoint
/// resources (exposures, metrics, semantic models, saved queries) ride the
/// same `direct_children`/`direct_parents` lists but are NOT a "use" — a
/// model consumed only by an exposure is still single-use.
pub(super) fn is_data_resource(resource_type: &str) -> bool {
    matches!(resource_type, "model" | "seed" | "snapshot" | "source")
}

/// A model that materializes as `view` or `ephemeral` — the
/// "no-durable-checkpoint" set `chained_views` and
/// `exposure_parents_materializations` share. A model with no materialization
/// recorded doesn't count (its storage shape is unknown, not virtual).
pub(super) fn is_view_or_ephemeral(n: &NodeInfo) -> bool {
    n.resource_type == "model" && matches!(n.materialized.as_deref(), Some("view" | "ephemeral"))
}

/// The PREFIX stage of [`lint_model_type`]: the configured layer whose
/// prefix the model name starts with, `None` when none matches. BTreeMap
/// order makes a name matching two layers' prefixes resolve to the
/// alphabetically-first layer — deterministic.
pub(super) fn lint_model_type_by_prefix<'a>(
    n: &NodeInfo,
    config: &'a LintConfig,
) -> Option<&'a str> {
    for (layer, prefixes) in &config.naming_prefixes {
        if prefixes.iter().any(|p| n.name.starts_with(p.as_str())) {
            return Some(layer.as_str());
        }
    }
    None
}

/// The DIRECTORY stage of [`lint_model_type`]: the model's [`first_dir`]
/// folder when it is itself a configured layer — `None` when the node has no
/// path or its first dir isn't a named layer (an unmapped dir is not a type —
/// the evaluator maps it to 'other', not to an implicit layer).
pub(super) fn lint_model_type_by_dir<'a>(n: &NodeInfo, config: &'a LintConfig) -> Option<&'a str> {
    let dir = first_dir(n)?;
    config
        .naming_prefixes
        .get_key_value(dir)
        .map(|(layer, _)| layer.as_str())
}

/// Resolve a model's lint layer the way dbt_project_evaluator resolves
/// `model_type` — the two stages above in order, then `"other"`:
///
/// 1. [`lint_model_type_by_prefix`] — a [`naming_prefixes`](LintConfig::naming_prefixes)
///    layer whose configured prefix the model name starts with;
/// 2. [`lint_model_type_by_dir`] — the [`first_dir`] folder, when it is
///    itself a configured layer;
/// 3. `"other"`, the evaluator's catch-all.
///
/// Returning the CONFIG KEY (not the dir) keeps the result comparable across
/// consumers: `marts_depend_on_source` matches it against `marts` /
/// `intermediate`, and `naming_conventions` looks the expected prefixes back
/// up in the same map. The `"other"` type only carries prefixes when the
/// config defines an `other:` layer (evaluator-parity `rpt_` enforcement is
/// opt-in — see the README's Differences section).
pub(super) fn lint_model_type<'a>(n: &NodeInfo, config: &'a LintConfig) -> &'a str {
    lint_model_type_by_prefix(n, config)
        .or_else(|| lint_model_type_by_dir(n, config))
        .unwrap_or("other")
}
