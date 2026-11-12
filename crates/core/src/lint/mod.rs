//! Static project-structure lint: best-practice checks over the project
//! graph, evaluated deterministically against the pruned
//! [`Dag`] — no warehouse execution and no `dbt compile` needed (a manifest
//! and a source-mode synthesized manifest produce the same `Dag`, so both
//! modes lint identically).
//!
//! ONE [`Finding`] vector feeds every surface — [`run_rules`] is the single
//! evaluation entry point shared by `dbtl check --lint`, the TUI's `Lint`
//! lens / stats worklist, and `dbtl docs` — so the numbers can never drift
//! the way a per-surface re-implementation would.
//!
//! Rule metadata lives in the [`REGISTRY`] table — the ONE list of what each
//! rule is called, requires, defaults to and accepts as parameters. The
//! `.dbtl.yml` file shape and its discovery live in `crate::project_config`;
//! this module owns the in-memory [`LintConfig`] `run_rules` consumes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::layer::first_dir;
use crate::{has_pk_guarantee, in_package, layer_violation_edges, Dag, NodeInfo};

mod baseline;
pub use baseline::{Baseline, BASELINE_SCHEMA, DEFAULT_BASELINE_NAME};

mod registry;
pub use registry::{
    removed_rule, removed_rule_error, Category, ParamKind, ParamSpec, RemovedRule, Requires,
    RuleInfo, Severity, REGISTRY, REMOVED_RULES,
};

mod call_sites;
pub use call_sites::{display_name, display_names, name_list};
mod location;
pub use location::{
    byte_span_position, fill_finding_lines, yaml_name_line, yaml_name_span, YAML_ATTRIBUTE_RULE_IDS,
};

mod rules;
pub use rules::{run_rules, LintInput, LintOutcome, SkippedRule};

/// A static structure-check rule, identified by its stable snake_case
/// name. The enum's declaration order IS the evaluation/output order of
/// [`run_rules`] (drift-tested against [`REGISTRY`]) — fixed, so finding
/// lists are deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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
    /// A source is declared but never referenced.
    UnusedSources,
    /// Two or more sources point at the same warehouse relation.
    DuplicateSources,
    /// A staging model directly depends on another staging model.
    StagingDependsOnStaging,
    /// A model directly depends on too many data resources.
    TooManyJoins,
    /// A chain of consecutive view/ephemeral models is too long.
    ChainedViews,
    /// An exposure directly depends on a source or a view/ephemeral model.
    ExposureParentsMaterializations,
    /// An exposure directly depends on a private/protected model.
    ExposuresDependentOnPrivateModels,
    /// A source has no description.
    UndocumentedSources,
    /// A model has declared columns without descriptions.
    UndocumentedColumns,
    /// A model's name prefix and directory resolve to different layers.
    ModelDirectories,
    /// A model is missing a configured `meta` key.
    RequiredMetaKeys,
    /// A contract-enforced model has declared columns without `data_type`.
    ContractColumnsWithoutType,
    /// An `incremental` model has no `unique_key`.
    IncrementalWithoutUniqueKey,
    /// A source has no effective `warn_after`/`error_after` freshness.
    SourcesWithoutFreshness,
    /// A model's outermost projection reads `*`/`t.*` (needs parsed SQL).
    SelectStar,
    /// A model names a relation literally instead of `ref()`/`source()`.
    HardCodedReferences,
    /// A JOIN carries no join condition (`CROSS JOIN` or bare `JOIN`).
    CartesianJoin,
    /// A model names a column a trusted upstream provably doesn't emit.
    UnknownColumnReference,
    /// A model's raw SQL matches a configured forbidden regex.
    ForbiddenPatterns,
    /// Declared column `data_type`s disagree with the catalog's types.
    WarehouseTypeDrift,
    /// Declared columns the catalog doesn't know (needs a catalog).
    ColumnsMissingInWarehouse,
    /// A model in a configured layer (default staging/intermediate) has no
    /// downstream — no node, exposure, or `semantic_models:` consumer.
    UnusedModels,
    /// A node directly depends on a model carrying `deprecation_date` —
    /// a deprecation scheduled upstream it should migrate off.
    ReferencesDeprecatedModel,
    /// Warehouse columns the catalog reports but the node doesn't declare
    /// (needs a catalog) — docs/schema.yml coverage, not a type question.
    UndeclaredWarehouseColumns,
    /// A model's SQL file effectively ends with `;` — the terminator
    /// breaks the `create … as (…)` dbt wraps the model in.
    TrailingSemicolon,
    /// A model's output-defining SELECT emits the same column name twice.
    DuplicateOutputColumns,
    /// A `WITH` CTE nothing in the model reads.
    UnusedCte,
    /// A model's final query has `ORDER BY` with no LIMIT/FETCH/TOP.
    OrderByWithoutLimit,
}

impl LintRule {
    /// Every rule, in the fixed evaluation/output order — the declaration
    /// order of the enum and of [`REGISTRY`]. The original structure rules
    /// lead; rules added later append in registry order so the emitted
    /// sequence stays stable.
    pub const ALL: [LintRule; 40] = [
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
        Self::UnusedSources,
        Self::DuplicateSources,
        Self::StagingDependsOnStaging,
        Self::TooManyJoins,
        Self::ChainedViews,
        Self::ExposureParentsMaterializations,
        Self::ExposuresDependentOnPrivateModels,
        Self::UndocumentedSources,
        Self::UndocumentedColumns,
        Self::ModelDirectories,
        Self::RequiredMetaKeys,
        Self::ContractColumnsWithoutType,
        Self::IncrementalWithoutUniqueKey,
        Self::SourcesWithoutFreshness,
        Self::SelectStar,
        Self::HardCodedReferences,
        Self::CartesianJoin,
        Self::UnknownColumnReference,
        Self::ForbiddenPatterns,
        Self::WarehouseTypeDrift,
        Self::ColumnsMissingInWarehouse,
        Self::UnusedModels,
        Self::ReferencesDeprecatedModel,
        Self::UndeclaredWarehouseColumns,
        Self::TrailingSemicolon,
        Self::DuplicateOutputColumns,
        Self::UnusedCte,
        Self::OrderByWithoutLimit,
    ];

    /// This rule's registry row — every rule has exactly one (drift-tested).
    pub fn info(self) -> &'static RuleInfo {
        REGISTRY
            .iter()
            .find(|i| i.rule == self)
            .expect("every LintRule has a registry row")
    }

    /// The stable snake_case id — reads the registry, so the `.dbtl.yml`
    /// key, `Finding.rule`'s serialization, `dbtl check --rules`' argument
    /// and the baseline's `rule` field are all the same string by
    /// construction.
    pub fn name(self) -> &'static str {
        self.info().id
    }

    /// The inverse of [`name`](LintRule::name) — `.dbtl.yml` validation and
    /// `check --rules` parsing. `None` for unknown names (the caller lists
    /// [`ALL`](LintRule::ALL) in its error).
    pub fn from_name(name: &str) -> Option<LintRule> {
        REGISTRY.iter().find(|i| i.id == name).map(|i| i.rule)
    }

    /// One line describing what the rule flags — the registry's `summary`,
    /// for docs/README output.
    pub fn description(self) -> &'static str {
        self.info().summary
    }

    /// This rule's anchor on the lint rules reference —
    /// `<docs site>/reference/lint-rules/#<id>`: the json report's
    /// `docs_url`, SARIF's `helpUri` and `--explain`'s `Docs:` line.
    pub fn docs_url(self) -> String {
        format!("{LINT_RULE_DOCS_BASE}#{}", self.name())
    }
}

/// Where a finding happened. `file` is the project-relative
/// `original_file_path`; `line` and `column` are the 1-based start of the
/// position a UI could jump to, `end_line`/`end_column` the range's
/// exclusive end (one `char` past the last character — the convention
/// [`byte_span_position`] returns). Columns count `char`s: a tab is one.
/// Every position field stays `None` when a rule can't pin it exactly — a
/// file-scoped finding carries `file` alone, never a guessed position.
/// For nodes declared in a properties yml (`patch_path`), `file` is that
/// yml, not the model's `.sql`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
}

impl Location {
    /// A location that names `file` only — every position unknown.
    pub fn new(file: impl Into<String>) -> Self {
        Location {
            file: file.into(),
            line: None,
            column: None,
            end_line: None,
            end_column: None,
        }
    }
}

/// A secondary position inside a finding's own file — the other spots a
/// one-finding-per-node rule points at from the finding's primary position
/// (the other `*` selects, the other literals a `hard_coded_references`
/// finding rolls up). `line` is required — a label without a line renders
/// nothing; `column` and the exclusive `end_line`/`end_column` are
/// optional (a line-level note carries no span).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Label {
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
    pub message: String,
}

/// The docs site's lint-rules page — the ONE base every per-rule link
/// builds on: `--explain`'s `Docs:` line, the json report's `docs_url`
/// and SARIF's `helpUri`.
pub const LINT_RULE_DOCS_BASE: &str = "https://hytfjwr.github.io/dbtl/reference/lint-rules/";

/// One rule violation on one node: the violating node, the related nodes for
/// context (a layer violation's parent, a rejoin's C+A endpoints, a fanout's
/// children — sorted), a human-readable explanation, and the envelope
/// (`severity`/`location`/`baselined`) `run_rules` fills in one place.
///
/// `label` and `labels` are the pointer annotations — the short text the
/// primary position's underline shows (`label`) and secondary positions in
/// the same file (`labels`, kept sorted by position). `help` is
/// finding-specific fix guidance; `None` means the rule's registry `help`
/// applies — [`Finding::effective_help`] resolves that default.
///
/// `check --lint` gates on these, the `Lint` lens tints `node_uid`, the stats
/// worklist lists them per rule, and docs pages quote `message` verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The rule that fired.
    pub rule: LintRule,
    /// The violating node — for EDGE findings this is the CHILD endpoint.
    /// `exclude:` selectors and `meta.dbtl.ignore` suppression match on this
    /// uid.
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
    /// The severity the finding ran at — the config's `severity` override
    /// or the registry's `default_severity`.
    pub severity: Severity,
    /// The node's project file when known (`original_file_path`, or the
    /// declaring yml for attribute rules); omitted from JSON when absent.
    /// A rule may attach its own position via [`Finding::with_span`];
    /// `run_rules` fills `file` only and drops a position whose file
    /// disagrees with the one it resolves.
    pub location: Option<Location>,
    /// The short text the primary position's underline carries (the name
    /// that "declared here" points at, a `select *` marker) — `None` when
    /// there is no position to annotate.
    pub label: Option<String>,
    /// Secondary positions in `location.file`, sorted by position — the
    /// other spots the rule folded into this one finding.
    pub labels: Vec<Label>,
    /// Finding-specific fix guidance — `None` falls back to the rule's
    /// registry `help` (see [`Finding::effective_help`]).
    pub help: Option<String>,
    /// `true` when a baseline entry matched — set by [`Baseline::apply`],
    /// never by `run_rules` itself.
    pub baselined: bool,
}

impl Finding {
    /// A bare finding in `rule`'s registry-default envelope. Rule functions
    /// build these; `run_rules` fills `severity`/`location` from the config
    /// and the dag afterward, in one place.
    pub fn new(
        rule: LintRule,
        node_uid: impl Into<String>,
        related: Vec<String>,
        message: impl Into<String>,
    ) -> Self {
        Finding {
            rule,
            node_uid: node_uid.into(),
            related,
            message: message.into(),
            severity: rule.info().default_severity,
            location: None,
            label: None,
            labels: Vec::new(),
            help: None,
            baselined: false,
        }
    }

    /// Attach the primary position: `file` and the 1-based
    /// `(line, column)`–`(end_line, end_column)` span the finding's pointer
    /// renders — `end_*` exclusive, matching [`byte_span_position`].
    /// `run_rules` keeps the position only while its `file` agrees with the
    /// file the envelope resolves.
    pub fn with_span(
        mut self,
        file: impl Into<String>,
        line: u32,
        column: u32,
        end_line: u32,
        end_column: u32,
    ) -> Self {
        self.location = Some(Location {
            file: file.into(),
            line: Some(line),
            column: Some(column),
            end_line: Some(end_line),
            end_column: Some(end_column),
        });
        self
    }

    /// The short text the primary position's underline carries.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Secondary positions in the same file — sorted by position, the
    /// order the reports render them.
    pub fn with_labels(mut self, labels: Vec<Label>) -> Self {
        self.labels = labels;
        self.labels
            .sort_by_key(|l| (l.line, l.column, l.end_line, l.end_column));
        self
    }

    /// Finding-specific fix guidance — overrides the rule's registry
    /// `help` for this one finding.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// The fix guidance shown for this finding — its own `help` when a
    /// rule attached one, else the rule's registry `help`.
    pub fn effective_help(&self) -> &str {
        self.help.as_deref().unwrap_or(self.rule.info().help)
    }

    /// The identity used by `check --lint --diff`'s "new violations only"
    /// comparison and by the lint baseline. For EDGE findings
    /// (`layer_violation`, `rejoining_upstream`) the other endpoint(s) are
    /// part of the identity — the violation IS the edge. For node-scoped
    /// findings the rule + node is the identity: a model that was already
    /// violating stays "pre-existing" even when its implicated neighbours
    /// changed (e.g. it joined one more source), so `--diff` doesn't re-flag
    /// stale violations as new.
    pub fn key(&self) -> (LintRule, &str, &[String]) {
        match self.rule {
            LintRule::LayerViolation | LintRule::RejoiningUpstream => {
                (self.rule, self.node_uid.as_str(), &self.related)
            }
            _ => (self.rule, self.node_uid.as_str(), &[]),
        }
    }
}

// The report wire shape — written out by hand so the json format can emit
// fields `Finding` doesn't store: `help` is always the EFFECTIVE help (the
// registry default when the finding carries none) and `docs_url` is
// computed from the rule id. Every pre-existing key keeps its slot;
// `label`/`labels`/`help`/`docs_url` append after `baselined`, and
// unknown positions stay absent rather than serializing as `null`.
impl Serialize for Finding {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct(
            "Finding",
            8 + self.location.is_some() as usize
                + self.label.is_some() as usize
                + !self.labels.is_empty() as usize,
        )?;
        st.serialize_field("rule", &self.rule)?;
        st.serialize_field("node_uid", &self.node_uid)?;
        st.serialize_field("related", &self.related)?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("severity", &self.severity)?;
        if let Some(location) = &self.location {
            st.serialize_field("location", location)?;
        }
        st.serialize_field("baselined", &self.baselined)?;
        if let Some(label) = &self.label {
            st.serialize_field("label", label)?;
        }
        if !self.labels.is_empty() {
            st.serialize_field("labels", &self.labels)?;
        }
        st.serialize_field("help", self.effective_help())?;
        st.serialize_field("docs_url", &self.rule.docs_url())?;
        st.end()
    }
}

/// `.dbtl.yml`'s `lint:` section, resolved over the shipped defaults. The
/// file schema lives in `crate::project_config` — this is the in-memory
/// shape [`run_rules`] consumes: per-rule severities (an absent entry =
/// the registry default, `None` = off), per-rule `exclude:` selectors, the
/// validated integer/string-list parameters (regex lists included — the
/// loader validated them), and the naming-prefix map a project merges
/// per-layer over the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintConfig {
    /// rule → `Some(severity)` pinned severity, `None` off. Absent = the
    /// registry's `default_severity`. `.dbtl.yml`'s `false` and
    /// `{severity: off}` both land as `None` here.
    pub severities: BTreeMap<LintRule, Option<Severity>>,
    /// rule → `exclude:` selector strings; findings whose `node_uid`
    /// matches the union are dropped.
    pub exclude: BTreeMap<LintRule, Vec<String>>,
    /// rule → declared integer parameter values (validated against the
    /// registry's [`ParamSpec`] at parse — only declared names land here).
    pub params: BTreeMap<LintRule, BTreeMap<String, usize>>,
    /// rule → declared string-list parameter values
    /// (`required_meta_keys.keys`, `missing_primary_key_tests.resource_types`)
    /// — validated against the registry's [`ParamSpec`] like the integer map.
    pub list_params: BTreeMap<LintRule, BTreeMap<String, Vec<String>>>,
    /// rule → declared boolean parameter values
    /// (`missing_primary_key_tests.accept_primary_key_constraint`) —
    /// validated against the registry's [`ParamSpec`] like the integer map.
    pub bool_params: BTreeMap<LintRule, BTreeMap<String, bool>>,
    /// `naming_conventions`: model TYPE → allowed name prefixes, where the
    /// type is resolved by `lint_model_type` (prefix-first, then
    /// [`first_dir`] folder, else `other`). A model whose resolved type is
    /// listed must start with one of its prefixes; a type absent from the
    /// map (or mapped to `[]`) is unchecked. `other` is a valid key: mapping
    /// it to e.g. `["rpt_"]` opts into flagging unclassified models without
    /// that prefix.
    pub naming_prefixes: BTreeMap<String, Vec<String>>,
    /// `lint.baseline` — the frozen-violations file path, resolved against
    /// the `.dbtl.yml` directory at load (the default
    /// [`DEFAULT_BASELINE_NAME`] when unset).
    pub baseline: PathBuf,
}

impl Default for LintConfig {
    /// The shipped defaults: every rule at its registry severity, no
    /// excludes, the conventional layer naming prefixes, and the default
    /// baseline path.
    fn default() -> Self {
        LintConfig {
            severities: BTreeMap::new(),
            exclude: BTreeMap::new(),
            params: BTreeMap::new(),
            list_params: BTreeMap::new(),
            bool_params: BTreeMap::new(),
            naming_prefixes: BTreeMap::from([
                ("staging".to_string(), vec!["stg_".to_string()]),
                ("intermediate".to_string(), vec!["int_".to_string()]),
                (
                    "marts".to_string(),
                    vec!["fct_".to_string(), "dim_".to_string(), "rpt_".to_string()],
                ),
            ]),
            baseline: PathBuf::from(DEFAULT_BASELINE_NAME),
        }
    }
}

impl LintConfig {
    /// The severity `rule` runs at, or `None` when the config turns it off
    /// (`false` / `severity: off`). Absent entries fall back to the
    /// registry's `default_severity`.
    pub fn severity(&self, rule: LintRule) -> Option<Severity> {
        self.severities
            .get(&rule)
            .copied()
            .unwrap_or_else(|| Some(rule.info().default_severity))
    }

    /// Whether `rule` runs under this config — i.e. its effective severity
    /// is not off.
    pub fn enabled(&self, rule: LintRule) -> bool {
        self.severity(rule).is_some()
    }

    /// `rule`'s `exclude:` selectors (empty when unset).
    pub fn excludes(&self, rule: LintRule) -> &[String] {
        self.exclude.get(&rule).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The effective value of `rule`'s integer parameter `param`: the
    /// configured override, else the registry-declared default. `None` only
    /// when `rule` declares no integer parameter of that name — the config
    /// parser rejects undeclared keys before they can land here.
    pub fn int_param(&self, rule: LintRule, param: &str) -> Option<usize> {
        self.params
            .get(&rule)
            .and_then(|m| m.get(param))
            .copied()
            .or_else(|| rule.info().int_param(param).map(|(_, default)| default))
    }

    /// The effective value of `rule`'s string-list parameter `param` — a
    /// configured override else the registry-declared default — covering
    /// BOTH `StringList` and `RegexList` param kinds (the config parser
    /// already validated a `RegexList`'s entries compile). `None` only when
    /// `rule` declares no list parameter of that name.
    pub fn str_list_param(&self, rule: LintRule, param: &str) -> Option<Vec<String>> {
        self.list_params
            .get(&rule)
            .and_then(|m| m.get(param))
            .cloned()
            .or_else(|| {
                rule.info()
                    .str_list_param(param)
                    .map(|(default, _)| default.iter().map(|s| s.to_string()).collect())
            })
    }

    /// The effective value of `rule`'s boolean parameter `param` — a
    /// configured override else the registry-declared default. `None` only
    /// when `rule` declares no boolean parameter of that name.
    pub fn bool_param(&self, rule: LintRule, param: &str) -> Option<bool> {
        self.bool_params
            .get(&rule)
            .and_then(|m| m.get(param))
            .copied()
            .or_else(|| rule.info().bool_param(param))
    }
}

#[cfg(test)]
mod tests;
