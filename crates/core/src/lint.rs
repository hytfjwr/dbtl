//! Static project-structure lint: the `dbt_project_evaluator`-style
//! best-practice checks, evaluated deterministically against the pruned
//! [`Dag`] — no warehouse execution and no `dbt compile` needed (a manifest
//! and a source-mode synthesized manifest produce the same `Dag`, so both
//! modes lint identically).
//!
//! ONE [`Finding`] vector feeds every surface — [`run_rules`] is the single
//! evaluation entry point shared by `dbtl check --lint`, the TUI's `Lint`
//! lens / stats worklist, and `dbtl docs` — so the numbers can never drift
//! the way a per-surface re-implementation would.
//!
//! Configuration lives in the project-local `.dbtl.yml` (a `lint:` section):
//! rule enable/disable, the fanout thresholds, and the naming prefixes are
//! adjustable there. It is deliberately NOT `~/.config/dbtl/config.yml` —
//! lint policy is project-owned (a CI gate must not depend on the
//! developer's machine).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layer::first_dir;
use crate::{in_package, layer_violation_edges, Dag, NodeInfo};

/// The conventional project-local config file name: `.dbtl.yml` next to
/// `dbt_project.yml` (or at the project root above `target/` in manifest
/// mode). Only the `lint:` section is read today; the file is a namespace
/// for dbtl project-level settings, not user-level ones.
pub const LINT_CONFIG_NAME: &str = ".dbtl.yml";

/// A static structure-check rule, identified by its `dbt_project_evaluator`
/// name. The enum's declaration order IS the evaluation/output order of
/// [`run_rules`] — fixed, so finding lists are deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LintRule {
    /// A model directly depends on both a source and a model.
    DirectJoinToSource,
    /// A model directly depends on two or more sources.
    MultipleSourcesJoined,
    /// A source is directly depended on by at least a threshold of models.
    SourceFanout,
    /// A model has at least a threshold of leaf-model children.
    ModelFanout,
    /// A model has no parents at all.
    RootModels,
    /// A→B→C and A→C where B is otherwise unused (a redundant hop).
    RejoiningUpstream,
    /// A marts/intermediate model directly depends on a source.
    MartsDependOnSource,
    /// A model lacks `unique` + `not_null` on the same column, or a
    /// `unique_combination_of_columns` test.
    MissingPrimaryKeyTests,
    /// `access: public` without `contract: enforced`.
    PublicModelsWithoutContract,
    /// A model carries no description.
    UndocumentedModels,
    /// A model's name misses its layer's required prefix(es).
    NamingConventions,
    /// A backward (e.g. marts→staging) edge — the shared
    /// [`layer_violation_edges`] predicate, surfaced as a lint rule.
    LayerViolation,
}

impl LintRule {
    /// Every rule, in the fixed evaluation/output order — the declaration
    /// order of the enum.
    pub const ALL: [LintRule; 12] = [
        Self::DirectJoinToSource,
        Self::MultipleSourcesJoined,
        Self::SourceFanout,
        Self::ModelFanout,
        Self::RootModels,
        Self::RejoiningUpstream,
        Self::MartsDependOnSource,
        Self::MissingPrimaryKeyTests,
        Self::PublicModelsWithoutContract,
        Self::UndocumentedModels,
        Self::NamingConventions,
        Self::LayerViolation,
    ];

    /// The evaluator-style rule name (what `.dbtl.yml` keys, `Finding.rule`
    /// serializes, and `dbtl check --rules` accepts).
    pub fn name(self) -> &'static str {
        match self {
            Self::DirectJoinToSource => "direct_join_to_source",
            Self::MultipleSourcesJoined => "multiple_sources_joined",
            Self::SourceFanout => "source_fanout",
            Self::ModelFanout => "model_fanout",
            Self::RootModels => "root_models",
            Self::RejoiningUpstream => "rejoining_upstream",
            Self::MartsDependOnSource => "marts_depend_on_source",
            Self::MissingPrimaryKeyTests => "missing_primary_key_tests",
            Self::PublicModelsWithoutContract => "public_models_without_contract",
            Self::UndocumentedModels => "undocumented_models",
            Self::NamingConventions => "naming_conventions",
            Self::LayerViolation => "layer_violation",
        }
    }

    /// The inverse of [`name`](LintRule::name) — `.dbtl.yml` validation and
    /// `check --rules` parsing. `None` for unknown names (the caller lists
    /// [`ALL`](LintRule::ALL) in its error).
    pub fn from_name(name: &str) -> Option<LintRule> {
        Self::ALL.into_iter().find(|r| r.name() == name)
    }

    /// One line describing what the rule flags, for docs/README output.
    pub fn description(self) -> &'static str {
        match self {
            Self::DirectJoinToSource => "a model directly depends on both a source and a model",
            Self::MultipleSourcesJoined => "a model directly depends on 2+ sources",
            Self::SourceFanout => "a source directly feeds too many models",
            Self::ModelFanout => "a model directly feeds too many leaf models",
            Self::RootModels => "a model has no parents",
            Self::RejoiningUpstream => "a redundant hop: A→B→C while A→C already exists",
            Self::MartsDependOnSource => "a marts/intermediate model directly depends on a source",
            Self::MissingPrimaryKeyTests => {
                "a model lacks a primary-key test (unique+not_null on one column, \
                 or unique_combination_of_columns)"
            }
            Self::PublicModelsWithoutContract => "access: public without contract: enforced",
            Self::UndocumentedModels => "a model has no description",
            Self::NamingConventions => "a model's name misses its layer's prefix",
            Self::LayerViolation => "a backward edge between logical layers",
        }
    }
}

/// One rule violation on one node: the violating node, the related nodes for
/// context (a layer violation's parent, a rejoin's A/C endpoints — sorted),
/// and a human-readable explanation.
///
/// `check --lint` gates on these, the `Lint` lens tints `node_uid`, the stats
/// worklist lists them per rule, and docs pages quote `message` verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// The rule that fired.
    pub rule: LintRule,
    /// The violating node.
    pub node_uid: String,
    /// Other uids implicated by the violation (sorted). Empty for node-scoped
    /// rules — their implicated neighbours ride the `message` instead, so the
    /// finding's identity stays the node itself.
    pub related: Vec<String>,
    /// The stable human-readable explanation shown in check output and docs.
    pub message: String,
}

impl Finding {
    /// The identity used by `check --lint --diff`'s "new violations only"
    /// comparison. For EDGE findings (`layer_violation`,
    /// `rejoining_upstream`) the other endpoint(s) are part of the identity —
    /// the violation IS the edge. For node-scoped findings the rule + node is
    /// the identity: a model that was already violating stays "pre-existing"
    /// even when its implicated neighbours changed (e.g. it joined one more
    /// source), so `--diff` doesn't re-flag stale violations as new.
    pub fn key(&self) -> (LintRule, &str, &[String]) {
        match self.rule {
            LintRule::LayerViolation | LintRule::RejoiningUpstream => {
                (self.rule, self.node_uid.as_str(), &self.related)
            }
            _ => (self.rule, self.node_uid.as_str(), &[]),
        }
    }
}

/// The `lint:` section of `.dbtl.yml`: which rules run (all enabled by
/// default), the two fanout thresholds, and the naming-prefix map. A rule
/// absent from `rules` is enabled; `naming_prefixes` merges per-layer over
/// the defaults — a project adjusts one layer without restating the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintConfig {
    /// rule name → enabled. Absent = enabled (every rule ships on).
    pub rules: BTreeMap<String, bool>,
    /// `model_fanout`: a model with AT-OR-ABOVE this many direct leaf-model
    /// children is flagged. Defaults to [`crate::PR_HUB_MIN_DOWNSTREAM`] so
    /// the "hub" notion matches the PR-impact hub risk flag. (The evaluator
    /// uses `> 3`; we flag `>=` per the proposal's 閾値以上 wording.)
    pub model_fanout_threshold: usize,
    /// `source_fanout`: a source with AT-OR-ABOVE this many direct model
    /// children is flagged. Default 2 (the evaluator's ">1").
    pub source_fanout_threshold: usize,
    /// `naming_conventions`: layer dir → allowed name prefixes. A model whose
    /// [`first_dir`] layer is listed must start with one of its prefixes; a
    /// layer absent from the map (or mapped to `[]`) is unchecked — the
    /// project's own directory taxonomy is its own business.
    pub naming_prefixes: BTreeMap<String, Vec<String>>,
}

impl Default for LintConfig {
    /// The shipped defaults: every rule enabled, evaluator-matched naming
    /// prefixes, and hub-aligned fanout thresholds.
    fn default() -> Self {
        LintConfig {
            rules: BTreeMap::new(),
            model_fanout_threshold: crate::PR_HUB_MIN_DOWNSTREAM,
            source_fanout_threshold: 2,
            naming_prefixes: BTreeMap::from([
                ("staging".to_string(), vec!["stg_".to_string()]),
                ("intermediate".to_string(), vec!["int_".to_string()]),
                (
                    "marts".to_string(),
                    vec!["fct_".to_string(), "dim_".to_string(), "rpt_".to_string()],
                ),
            ]),
        }
    }
}

/// The `.dbtl.yml` top-level shape. Only `lint:` is consumed today; unknown
/// top-level keys are ignored so the file can grow other project-level
/// sections without breaking older binaries.
#[derive(Debug, Default, Deserialize)]
struct DbtlFile {
    #[serde(default)]
    lint: LintFile,
}

/// The `lint:` section wire shape. `deny_unknown_fields` makes a typo'd key
/// a hard error — a silently-ignored `model_fanout_threhsold` would leave a
/// CI gate running the wrong rule set.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LintFile {
    /// rule name → enabled. Unknown rule names are validated separately so
    /// the error can list the valid set.
    rules: BTreeMap<String, bool>,
    model_fanout_threshold: Option<usize>,
    source_fanout_threshold: Option<usize>,
    /// layer dir → allowed prefixes; merges per-layer over the defaults.
    naming_prefixes: Option<BTreeMap<String, Vec<String>>>,
}

impl LintConfig {
    /// Whether `rule` runs under this config (absent = enabled).
    pub fn enabled(&self, rule: LintRule) -> bool {
        self.rules.get(rule.name()).copied().unwrap_or(true)
    }

    /// Parse `.dbtl.yml` text: apply the `lint:` section over the shipped
    /// defaults. Errors on malformed YAML, unknown keys inside `lint:`, and
    /// unknown rule names inside `rules:` — a config a CI gate can't
    /// interpret must fail loudly, never silently lint with defaults.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let file: DbtlFile =
            serde_norway::from_str(text).map_err(|e| format!("invalid .dbtl.yml: {e}"))?;
        let mut cfg = LintConfig::default();
        for (name, enabled) in file.lint.rules {
            if LintRule::from_name(&name).is_none() {
                return Err(format!(
                    "unknown lint rule '{name}' (valid rules: {})",
                    LintRule::ALL
                        .iter()
                        .map(|r| r.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            cfg.rules.insert(name, enabled);
        }
        if let Some(t) = file.lint.model_fanout_threshold {
            if t == 0 {
                return Err("model_fanout_threshold must be >= 1".to_string());
            }
            cfg.model_fanout_threshold = t;
        }
        if let Some(t) = file.lint.source_fanout_threshold {
            if t == 0 {
                return Err("source_fanout_threshold must be >= 1".to_string());
            }
            cfg.source_fanout_threshold = t;
        }
        // Per-layer merge: a project overrides only the layers it lists; a
        // layer mapped to `[]` is unchecked (absent == unchecked anyway, but
        // an explicit empty list also SHUTS OFF a default layer).
        if let Some(prefixes) = file.lint.naming_prefixes {
            for (layer, p) in prefixes {
                cfg.naming_prefixes.insert(layer, p);
            }
        }
        Ok(cfg)
    }

    /// Load `.dbtl.yml` from `path`: missing file → the shipped defaults (a
    /// project without config still lints), malformed → `InvalidData` with
    /// the parse reason. Whether a malformed config is fatal or warn+fallback
    /// is the caller's policy — the CI surfaces (`check`, `docs`) treat it as
    /// an error, the TUI warns and falls back.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_yaml(&text).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// The `.dbtl.yml` candidates for a data source, in priority order: the
/// source's own directory first (source mode's project dir; for a manifest,
/// its containing dir — `<root>/target` under the standard layout), then the
/// parent dir (the project root above `target/`). A manifest or project not
/// under the standard layout still gets a sensible 2-level search. The first
/// EXISTING file wins ([`lint_config_path`]); when none exists the first
/// candidate is still the conventional place to write one.
pub fn lint_config_candidates(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let Some(base) = manifest_path
        .and_then(Path::parent)
        .or(project_dir)
        .map(Path::to_path_buf)
    else {
        return Vec::new();
    };
    let mut out = vec![base.join(LINT_CONFIG_NAME)];
    if let Some(parent) = base.parent() {
        out.push(parent.join(LINT_CONFIG_NAME));
    }
    out
}

/// The `.dbtl.yml` actually in effect for a data source: the first candidate
/// that exists on disk ([`lint_config_candidates`] order), else `None` — the
/// caller then uses [`LintConfig::default`].
pub fn lint_config_path(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    lint_config_candidates(manifest_path, project_dir)
        .into_iter()
        .find(|p| p.is_file())
}

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
                marts_depend_on_source(dag, &uids, &in_scope, &mut out)
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
fn model_and_source_parents(dag: &Dag, uid: &str) -> (Vec<String>, Vec<String>) {
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
fn direct_join_to_source(
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
fn multiple_sources_joined(
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
fn source_fanout(
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
fn model_fanout(
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
fn root_models(
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

/// `rejoining_upstream`: model B whose ONLY child is model C, while one of
/// B's model parents A also feeds C directly (A→B→C plus A→C — B is a
/// redundant hop). One finding per (A,B,C) triple, `related` = `[A, C]`.
fn rejoining_upstream(
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
        let children = dag.direct_children(uid);
        // "Otherwise unused": exactly one child, and it's a model.
        let [c_uid] = children else { continue };
        if dag.get(c_uid).is_none_or(|c| c.resource_type != "model") {
            continue;
        }
        for a_uid in dag.direct_parents(uid) {
            if dag.get(a_uid).is_none_or(|a| a.resource_type != "model") {
                continue;
            }
            if !dag.direct_parents(c_uid).contains(a_uid) {
                continue;
            }
            let aname = dag.get(a_uid).map_or(a_uid.as_str(), |a| &a.name);
            let cname = dag.get(c_uid).map_or(c_uid.as_str(), |c| &c.name);
            out.push(Finding {
                rule: LintRule::RejoiningUpstream,
                node_uid: (*uid).clone(),
                related: vec![a_uid.clone(), (*c_uid).clone()],
                message: format!(
                    "{} is a redundant hop: {aname} already feeds {cname} directly",
                    n.name
                ),
            });
        }
    }
}

/// `marts_depend_on_source`: a marts/intermediate model directly depends on a
/// source (raw-source reads belong in staging). Any marts-layer model with a
/// source parent fires — unlike `direct_join_to_source`, no model parent is
/// required.
fn marts_depend_on_source(
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
        let Some(layer) = first_dir(n) else { continue };
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
fn missing_primary_key_tests(
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
        let tests = dag.tests(uid);
        let mut uniques: BTreeSet<&str> = BTreeSet::new();
        let mut not_nulls: BTreeSet<&str> = BTreeSet::new();
        let mut has_combination = false;
        for t in tests {
            match t.kind.as_str() {
                "unique" => {
                    if let Some(c) = &t.column_name {
                        uniques.insert(c);
                    }
                }
                "not_null" => {
                    if let Some(c) = &t.column_name {
                        not_nulls.insert(c);
                    }
                }
                kind if kind.ends_with("unique_combination_of_columns") => {
                    has_combination = true;
                }
                _ => {}
            }
        }
        if has_combination || !uniques.is_disjoint(&not_nulls) {
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
fn public_models_without_contract(
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
fn undocumented_models(
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

/// `naming_conventions`: a model whose `first_dir` layer is listed in
/// `naming_prefixes` must start with one of the listed prefixes. Layers
/// absent from the map are unchecked.
fn naming_conventions(
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
        let Some(layer) = first_dir(n) else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RawContract, RawManifest, RawNode, RawSource, RawTestMetadata};
    use std::collections::HashMap;

    /// A manifest assembled from just the interesting fields — the remaining
    /// maps default empty. `child_map` is derived from `edges` (parent, child)
    /// pairs so each test writes its graph ONCE.
    fn manifest(
        nodes: Vec<(String, RawNode)>,
        sources: Vec<(String, RawSource)>,
        edges: &[(&str, &str)],
    ) -> RawManifest {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for (p, c) in edges {
            parent_map
                .entry((*c).to_string())
                .or_default()
                .push((*p).to_string());
            child_map
                .entry((*p).to_string())
                .or_default()
                .push((*c).to_string());
        }
        RawManifest {
            metadata: Default::default(),
            nodes: nodes.into_iter().collect(),
            sources: sources.into_iter().collect(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map,
            child_map,
        }
    }

    /// A model node literal; `path` carries the layer dir (`staging/x.sql`).
    fn model(name: &str, path: &str) -> (String, RawNode) {
        (
            format!("model.p.{name}"),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(path.into()),
                ..Default::default()
            },
        )
    }

    /// A source node literal — `source.p.<src>.<tbl>` like a real manifest.
    fn source(name: &str) -> (String, RawSource) {
        (
            format!("source.p.{name}"),
            RawSource {
                name: name.into(),
                resource_type: "source".into(),
                ..Default::default()
            },
        )
    }

    /// A generic-test node literal attached to `target` (kind + column).
    fn test_on(target: &str, kind: &str, column: &str) -> (String, RawNode) {
        (
            format!("test.p.{kind}_{target}_{column}"),
            RawNode {
                name: format!("{kind}_{column}"),
                resource_type: "test".into(),
                attached_node: Some(format!("model.p.{target}")),
                column_name: Some(column.into()),
                test_metadata: Some(RawTestMetadata {
                    name: Some(kind.into()),
                }),
                ..Default::default()
            },
        )
    }

    /// The uids flagged by `rule` in `findings`, sorted for comparison.
    fn flagged(findings: &[Finding], rule: LintRule) -> Vec<&str> {
        let mut uids: Vec<&str> = findings
            .iter()
            .filter(|f| f.rule == rule)
            .map(|f| f.node_uid.as_str())
            .collect();
        uids.sort_unstable();
        uids
    }

    fn findings_of(m: &RawManifest, config: &LintConfig) -> Vec<Finding> {
        run_rules(&Dag::build(m), config)
    }

    #[test]
    fn default_config_enables_every_rule() {
        let cfg = LintConfig::default();
        for rule in LintRule::ALL {
            assert!(cfg.enabled(rule), "{} must default to enabled", rule.name());
        }
        assert_eq!(cfg.model_fanout_threshold, crate::PR_HUB_MIN_DOWNSTREAM);
        assert_eq!(cfg.source_fanout_threshold, 2);
    }

    #[test]
    fn yaml_disables_rules_and_overrides_thresholds() {
        let cfg = LintConfig::from_yaml(
            r#"lint:
  rules:
    undocumented_models: false
    source_fanout: false
  model_fanout_threshold: 3
  naming_prefixes:
    staging: [stg_, base_]
"#,
        )
        .unwrap();
        assert!(!cfg.enabled(LintRule::UndocumentedModels));
        assert!(!cfg.enabled(LintRule::SourceFanout));
        assert!(cfg.enabled(LintRule::RootModels));
        assert_eq!(cfg.model_fanout_threshold, 3);
        // Per-layer merge: staging overridden, intermediate/marts keep defaults.
        assert_eq!(
            cfg.naming_prefixes["staging"],
            vec!["stg_".to_string(), "base_".to_string()]
        );
        assert_eq!(
            cfg.naming_prefixes["intermediate"],
            vec!["int_".to_string()]
        );
    }

    #[test]
    fn yaml_rejects_unknown_rule_key_and_threshold_zero() {
        assert!(
            LintConfig::from_yaml("lint:\n  rules:\n    typo_rule: false\n")
                .unwrap_err()
                .contains("unknown lint rule 'typo_rule'")
        );
        assert!(LintConfig::from_yaml("lint:\n  nope: 1\n").is_err()); // deny_unknown_fields
        assert!(LintConfig::from_yaml("lint:\n  model_fanout_threshold: 0\n").is_err());
        assert!(LintConfig::from_yaml("lint: [not, a, map]\n").is_err()); // malformed
    }

    #[test]
    fn direct_join_to_source_flags_source_and_model_mix() {
        // bad joins src + stg_a; good joins only stg_a.
        let m = manifest(
            vec![
                model("stg_a", "staging/stg_a.sql"),
                model("bad", "staging/bad.sql"),
                model("good", "marts/good.sql"),
            ],
            vec![source("s.raw")],
            &[
                ("source.p.s.raw", "model.p.stg_a"),
                ("source.p.s.raw", "model.p.bad"),
                ("model.p.stg_a", "model.p.bad"),
                ("model.p.stg_a", "model.p.good"),
            ],
        );
        let f = findings_of(&m, &LintConfig::default());
        assert_eq!(
            flagged(&f, LintRule::DirectJoinToSource),
            vec!["model.p.bad"]
        );
    }

    #[test]
    fn multiple_sources_joined_needs_two_sources() {
        let m = manifest(
            vec![
                model("stg_two", "staging/stg_two.sql"),
                model("stg_one", "staging/stg_one.sql"),
            ],
            vec![source("a.t"), source("b.t")],
            &[
                ("source.p.a.t", "model.p.stg_two"),
                ("source.p.b.t", "model.p.stg_two"),
                ("source.p.a.t", "model.p.stg_one"),
            ],
        );
        let f = findings_of(&m, &LintConfig::default());
        assert_eq!(
            flagged(&f, LintRule::MultipleSourcesJoined),
            vec!["model.p.stg_two"]
        );
    }

    #[test]
    fn source_fanout_flags_source_with_two_model_children() {
        let m = manifest(
            vec![
                model("stg_a", "staging/stg_a.sql"),
                model("stg_b", "staging/stg_b.sql"),
            ],
            vec![source("s.hot"), source("s.cold")],
            &[
                ("source.p.s.hot", "model.p.stg_a"),
                ("source.p.s.hot", "model.p.stg_b"),
                ("source.p.s.cold", "model.p.stg_b"),
            ],
        );
        let f = findings_of(&m, &LintConfig::default());
        assert_eq!(flagged(&f, LintRule::SourceFanout), vec!["source.p.s.hot"]);
    }

    #[test]
    fn model_fanout_flags_leaf_children_at_threshold() {
        // hub feeds 2 leaf models; default threshold is 5 → no finding. With a
        // config of 2 the same graph flags hub. int_* keeps the names legal.
        let m = manifest(
            vec![
                model("int_hub", "intermediate/int_hub.sql"),
                model("fct_a", "marts/fct_a.sql"),
                model("fct_b", "marts/fct_b.sql"),
            ],
            vec![],
            &[
                ("model.p.int_hub", "model.p.fct_a"),
                ("model.p.int_hub", "model.p.fct_b"),
            ],
        );
        let base = Dag::build(&m);
        let default = run_rules(&base, &LintConfig::default());
        assert!(flagged(&default, LintRule::ModelFanout).is_empty());
        let cfg = LintConfig::from_yaml("lint:\n  model_fanout_threshold: 2\n").unwrap();
        let tightened = run_rules(&base, &cfg);
        assert_eq!(
            flagged(&tightened, LintRule::ModelFanout),
            vec!["model.p.int_hub"]
        );
    }

    #[test]
    fn root_models_flags_parentless_models_only() {
        let m = manifest(
            vec![
                model("stg_root", "staging/stg_root.sql"),
                model("int_child", "intermediate/int_child.sql"),
            ],
            vec![],
            &[("model.p.stg_root", "model.p.int_child")],
        );
        let f = findings_of(&m, &LintConfig::default());
        assert_eq!(flagged(&f, LintRule::RootModels), vec!["model.p.stg_root"]);
    }

    #[test]
    fn rejoining_upstream_flags_the_redundant_hop() {
        // a -> b -> c AND a -> c: b is a redundant hop. a -> d -> e with NO
        // a -> e shortcut: d is not flagged.
        let m = manifest(
            vec![
                model("stg_a", "staging/stg_a.sql"),
                model("int_b", "intermediate/int_b.sql"),
                model("fct_c", "marts/fct_c.sql"),
                model("int_d", "intermediate/int_d.sql"),
                model("fct_e", "marts/fct_e.sql"),
            ],
            vec![],
            &[
                ("model.p.stg_a", "model.p.int_b"),
                ("model.p.int_b", "model.p.fct_c"),
                ("model.p.stg_a", "model.p.fct_c"),
                ("model.p.stg_a", "model.p.int_d"),
                ("model.p.int_d", "model.p.fct_e"),
            ],
        );
        let f = findings_of(&m, &LintConfig::default());
        let hits: Vec<&Finding> = f
            .iter()
            .filter(|f| f.rule == LintRule::RejoiningUpstream)
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_uid, "model.p.int_b");
        assert_eq!(
            hits[0].related,
            vec!["model.p.stg_a".to_string(), "model.p.fct_c".to_string()]
        );
    }

    #[test]
    fn marts_depend_on_source_scopes_to_marts_and_intermediate() {
        // staging MAY read sources; a marts/intermediate direct read flags.
        let m = manifest(
            vec![
                model("stg_ok", "staging/stg_ok.sql"),
                model("fct_bad", "marts/fct_bad.sql"),
                model("int_bad", "intermediate/int_bad.sql"),
            ],
            vec![source("s.raw")],
            &[
                ("source.p.s.raw", "model.p.stg_ok"),
                ("source.p.s.raw", "model.p.fct_bad"),
                ("source.p.s.raw", "model.p.int_bad"),
                ("model.p.stg_ok", "model.p.fct_bad"),
            ],
        );
        let f = findings_of(&m, &LintConfig::default());
        let mut got = flagged(&f, LintRule::MartsDependOnSource);
        got.sort_unstable();
        assert_eq!(got, vec!["model.p.fct_bad", "model.p.int_bad"]);
    }

    #[test]
    fn missing_primary_key_tests_accepts_same_column_pair_or_combination() {
        // tested: unique+not_null on `id` → OK. combo: unique_combination → OK.
        // split: unique on a, not_null on b → still missing. bare: nothing.
        let m = manifest(
            vec![
                model("stg_tested", "staging/stg_tested.sql"),
                model("stg_combo", "staging/stg_combo.sql"),
                model("stg_split", "staging/stg_split.sql"),
                model("stg_bare", "staging/stg_bare.sql"),
                test_on("stg_tested", "unique", "id"),
                test_on("stg_tested", "not_null", "id"),
                test_on("stg_split", "unique", "a"),
                test_on("stg_split", "not_null", "b"),
                (
                    "test.p.ucombo".to_string(),
                    RawNode {
                        name: "ucombo".into(),
                        resource_type: "test".into(),
                        attached_node: Some("model.p.stg_combo".into()),
                        test_metadata: Some(RawTestMetadata {
                            name: Some("unique_combination_of_columns".into()),
                        }),
                        ..Default::default()
                    },
                ),
            ],
            vec![],
            &[],
        );
        let f = findings_of(&m, &LintConfig::default());
        let mut got = flagged(&f, LintRule::MissingPrimaryKeyTests);
        got.sort_unstable();
        assert_eq!(got, vec!["model.p.stg_bare", "model.p.stg_split"]);
    }

    #[test]
    fn public_models_without_contract_flags_public_uncontracted() {
        let mut nodes = vec![
            model("dim_pub", "marts/dim_pub.sql"),
            model("dim_contract", "marts/dim_contract.sql"),
            model("dim_private", "marts/dim_private.sql"),
        ];
        nodes[0].1.access = Some("public".into());
        nodes[1].1.access = Some("public".into());
        nodes[1].1.contract = Some(RawContract { enforced: true });
        nodes[2].1.access = Some("private".into());
        let m = manifest(nodes, vec![], &[]);
        let f = findings_of(&m, &LintConfig::default());
        assert_eq!(
            flagged(&f, LintRule::PublicModelsWithoutContract),
            vec!["model.p.dim_pub"]
        );
    }

    #[test]
    fn undocumented_models_flags_missing_and_blank_descriptions() {
        let mut nodes = vec![
            model("stg_doc", "staging/stg_doc.sql"),
            model("stg_bare", "staging/stg_bare.sql"),
            model("stg_blank", "staging/stg_blank.sql"),
        ];
        nodes[0].1.description = Some("documented".into());
        nodes[2].1.description = Some("   \n".into()); // cleaned to None at build
        let m = manifest(nodes, vec![], &[]);
        let f = findings_of(&m, &LintConfig::default());
        let mut got = flagged(&f, LintRule::UndocumentedModels);
        got.sort_unstable();
        assert_eq!(got, vec!["model.p.stg_bare", "model.p.stg_blank"]);
    }

    #[test]
    fn naming_conventions_checks_listed_layers_only() {
        // stg_-named marts model violates; unknown-layer dirs are unchecked.
        let m = manifest(
            vec![
                model("fct_ok", "marts/fct_ok.sql"),
                model("stg_wrong", "marts/stg_wrong.sql"),
                model("anything", "audit/anything.sql"),
            ],
            vec![],
            &[],
        );
        let f = findings_of(&m, &LintConfig::default());
        assert_eq!(
            flagged(&f, LintRule::NamingConventions),
            vec!["model.p.stg_wrong"]
        );
    }

    #[test]
    fn layer_violation_reuses_the_shared_predicate() {
        // stg_a depending on marts' fct_bad is the backward edge; stg->fct is
        // fine. node_uid is the model doing the bad dependency (the child).
        let m = manifest(
            vec![
                model("stg_a", "staging/stg_a.sql"),
                model("fct_a", "marts/fct_a.sql"),
                model("fct_bad", "marts/fct_bad.sql"),
            ],
            vec![],
            &[
                ("model.p.stg_a", "model.p.fct_a"),
                ("model.p.fct_bad", "model.p.stg_a"),
            ],
        );
        let f = findings_of(&m, &LintConfig::default());
        let hits: Vec<&Finding> = f
            .iter()
            .filter(|f| f.rule == LintRule::LayerViolation)
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_uid, "model.p.stg_a");
        assert_eq!(hits[0].related, vec!["model.p.fct_bad".to_string()]);
    }

    #[test]
    fn disabled_rule_produces_no_findings() {
        let m = manifest(vec![model("stg_root", "staging/stg_root.sql")], vec![], &[]);
        let cfg = LintConfig::from_yaml("lint:\n  rules:\n    root_models: false\n").unwrap();
        let f = findings_of(&m, &cfg);
        assert!(flagged(&f, LintRule::RootModels).is_empty());
        // Other rules still ran — undocumented_models fires on the same node.
        assert_eq!(
            flagged(&f, LintRule::UndocumentedModels),
            vec!["model.p.stg_root"]
        );
    }

    #[test]
    fn findings_are_deterministic_and_rule_ordered() {
        let m = manifest(
            vec![
                model("stg_a", "staging/stg_a.sql"),
                model("bad_name", "marts/bad_name.sql"),
            ],
            vec![source("s.raw")],
            &[("source.p.s.raw", "model.p.stg_a")],
        );
        let dag = Dag::build(&m);
        let a = run_rules(&dag, &LintConfig::default());
        let b = run_rules(&dag, &LintConfig::default());
        assert_eq!(a, b, "findings must be identical across runs");
        let rule_order: Vec<LintRule> = a.iter().map(|f| f.rule).collect();
        // The emitted order must follow LintRule::ALL — the ALL index of
        // consecutive findings is non-decreasing.
        let idx = |r: LintRule| LintRule::ALL.iter().position(|x| *x == r).unwrap();
        for w in rule_order.windows(2) {
            assert!(idx(w[0]) <= idx(w[1]));
        }
    }

    #[test]
    fn installed_package_nodes_are_not_flagged() {
        // model.pkg.other is an installed package's model: it must not flag
        // even though it has no parents/docs. PackageScope::All keeps it in
        // the graph; root-project scoping must still exclude it.
        let m = manifest(
            vec![
                model("stg_a", "staging/stg_a.sql"),
                (
                    "model.pkg.other".to_string(),
                    RawNode {
                        name: "other".into(),
                        resource_type: "model".into(),
                        path: Some("staging/other.sql".into()),
                        package_name: Some("pkg".into()),
                        ..Default::default()
                    },
                ),
            ],
            vec![],
            &[],
        );
        let mut m = m;
        m.metadata.project_name = Some("p".into());
        let dag = Dag::build(&m);
        assert!(dag.contains("model.pkg.other"), "package node kept in dag");
        let f = run_rules(&dag, &LintConfig::default());
        for rule in LintRule::ALL {
            assert!(
                flagged(&f, rule)
                    .iter()
                    .all(|u| !u.starts_with("model.pkg.")),
                "{} flagged an installed-package node",
                rule.name()
            );
        }
    }

    #[test]
    fn finding_key_distinguishes_edge_from_node_findings() {
        // Node findings key on (rule, node): changed related sets don't make a
        // pre-existing violation look "new" under --diff.
        let node_finding = Finding {
            rule: LintRule::DirectJoinToSource,
            node_uid: "model.p.x".into(),
            related: vec!["source.p.s.t".into()],
            message: String::new(),
        };
        assert_eq!(
            node_finding.key(),
            (LintRule::DirectJoinToSource, "model.p.x", &[][..])
        );
        let edge_finding = Finding {
            rule: LintRule::LayerViolation,
            node_uid: "model.p.c".into(),
            related: vec!["model.p.p".into()],
            message: String::new(),
        };
        assert_eq!(
            edge_finding.key(),
            (
                LintRule::LayerViolation,
                "model.p.c",
                &["model.p.p".to_string()][..]
            )
        );
    }

    #[test]
    fn lint_config_path_prefers_the_datasources_own_dir() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("target").join("manifest.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        // No .dbtl.yml anywhere → None.
        assert_eq!(lint_config_path(Some(&manifest), None), None);
        // `<root>/.dbtl.yml` (parent of target/) is found through the 2-level walk.
        let root_cfg = dir.path().join(LINT_CONFIG_NAME);
        std::fs::write(&root_cfg, "lint: {}\n").unwrap();
        assert_eq!(lint_config_path(Some(&manifest), None), Some(root_cfg));
        // Source mode looks in the project dir itself.
        let proj = tempfile::tempdir().unwrap();
        let cfg = proj.path().join(LINT_CONFIG_NAME);
        std::fs::write(&cfg, "lint: {}\n").unwrap();
        assert_eq!(lint_config_path(None, Some(proj.path())), Some(cfg));
    }
}
