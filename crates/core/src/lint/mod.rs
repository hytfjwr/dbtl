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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layer::first_dir;
use crate::{has_pk_guarantee, in_package, layer_violation_edges, Dag, NodeInfo};

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
/// context (a layer violation's parent, a rejoin's C+A endpoints, a fanout's
/// children — sorted), and a human-readable explanation.
///
/// `check --lint` gates on these, the `Lint` lens tints `node_uid`, the stats
/// worklist lists them per rule, and docs pages quote `message` verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// The rule that fired.
    pub rule: LintRule,
    /// The violating node.
    pub node_uid: String,
    /// Other uids implicated by the violation (sorted uid sets, not
    /// positional — for `rejoining_upstream` the merged {C}∪{A…} set, for
    /// `direct_join_to_source` sources-then-models; the `message` carries the
    /// per-endpoint semantics). Empty for node-scoped rules — their
    /// implicated neighbours ride the `message` instead, so the finding's
    /// identity stays the node itself.
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
    /// compares `>=` too — its default threshold is 3, ours 5; the operator
    /// is the same, only the default differs.)
    pub model_fanout_threshold: usize,
    /// `source_fanout`: a source with AT-OR-ABOVE this many direct model
    /// children is flagged. Default 2 (the evaluator's ">1").
    pub source_fanout_threshold: usize,
    /// `naming_conventions`: model TYPE → allowed name prefixes, where the
    /// type is resolved by `lint_model_type` (prefix-first, then
    /// [`first_dir`] folder, else `other`). A model whose resolved type is
    /// listed must start with one of its prefixes; a type absent from the
    /// map (or mapped to `[]`) is unchecked. `other` is a valid key: mapping
    /// it to e.g. `["rpt_"]` opts into the evaluator's default behaviour of
    /// flagging unclassified models without that prefix.
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
/// a hard error — a silently-ignored `model_fanout_threshld` would leave a
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

/// The `.dbtl.yml` candidates for a data source, in priority order:
///
/// - **Source mode** (`project_dir`): the project dir itself — it IS the
///   project root.
/// - **Manifest mode**: when the manifest sits at the standard
///   `<root>/target/manifest.json` layout (same heuristic
///   [`dag.rs`](crate::dag) uses for the staleness project-root), the
///   PROJECT ROOT wins first, then `target/` itself as an escape hatch. A
///   manifest anywhere else (`--manifest /tmp/x/manifest.json`) gets ONLY its
///   own directory — climbing to an unrelated parent's `.dbtl.yml` would lint
///   a copied artifact against a foreign config.
///
/// The first EXISTING file wins ([`lint_config_path`]); when none exists the
/// first candidate is still the conventional place to write one.
pub fn lint_config_candidates(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(dir) = manifest_path.and_then(Path::parent) {
        let mut out = Vec::new();
        // Standard layout: <root>/target/manifest.json → root first.
        if dir.file_name().is_some_and(|n| n == "target") {
            if let Some(root) = dir.parent() {
                out.push(root.join(LINT_CONFIG_NAME));
            }
        }
        out.push(dir.join(LINT_CONFIG_NAME));
        return out;
    }
    project_dir.map_or_else(Vec::new, |d| vec![d.join(LINT_CONFIG_NAME)])
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

mod rules;
#[cfg(test)]
mod tests;

pub use rules::run_rules;
