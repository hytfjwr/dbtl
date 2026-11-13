//! The rule implementations — one `pub(super)` function per rule, grouped into a
//! file per category so sibling rule groups extend their own file without
//! touching this one. `run_rules` below is the ONE evaluation entry point
//! `dbtl check --lint`, the TUI's Lint lens and `dbtl docs` all share; the
//! shared predicates (layer resolution, parent classification, the scope
//! test) also live here so no category file re-implements them.

use std::collections::BTreeSet;

use serde::Serialize;

use super::*;
use crate::select::select;
use crate::SqlDialect;
use crate::{Catalog, SqlParseResolver};

pub(super) use crate::lint::call_sites::{
    display_name, display_names, family_name, name_list, pluralize, version_families,
    CallSiteResolver,
};

mod documentation;
mod governance;
mod modeling;
mod performance;
mod sql;
mod structure;
mod testing;
mod warehouse;

/// The inputs a lint evaluation reads. `dag` and `config` are always
/// present; `sql` (a shared, lazily-parsing [`SqlParseResolver`]) and
/// `catalog` are optional inputs the caller supplies only when it owns
/// them — an enabled rule whose declared [`Requires`] has no input is
/// SKIPPED (recorded in [`LintOutcome::skipped`], never run), never a
/// failure.
#[derive(Clone, Copy)]
pub struct LintInput<'a> {
    pub dag: &'a Dag,
    pub config: &'a LintConfig,
    /// The shared SQL resolver [`Requires::Sql`] rules read facts from.
    /// Parsing is per-model lazy inside the resolver — passing one costs
    /// nothing until a rule asks.
    pub sql: Option<&'a SqlParseResolver>,
    /// The warehouse catalog [`Requires::Catalog`] rules read.
    pub catalog: Option<&'a Catalog>,
    /// The adapter context dialect-aware judging uses —
    /// `warehouse_type_drift`'s type-alias table selection. Resolved in
    /// [`LintInput::new`] like every caller's "auto" path (`--sql-dialect`
    /// wins via [`LintInput::with_dialect`], else the manifest's adapter,
    /// else [`SqlDialect::Generic`]); [`LintInput::with_sql`] adopts the
    /// resolver's own dialect so both sides of a diff agree.
    pub dialect: SqlDialect,
}

impl<'a> LintInput<'a> {
    pub fn new(dag: &'a Dag, config: &'a LintConfig) -> Self {
        LintInput {
            dag,
            config,
            sql: None,
            catalog: None,
            dialect: SqlDialect::from_dag(dag),
        }
    }

    /// Supply the SQL resolver (`Requires::Sql` rules then evaluate). The
    /// resolver's own dialect becomes the input's adapter context — the
    /// SQL rules and the catalog rules can never parse under different
    /// dialects.
    pub fn with_sql(mut self, sql: Option<&'a SqlParseResolver>) -> Self {
        self.sql = sql;
        if let Some(r) = sql {
            self.dialect = r.dialect();
        }
        self
    }

    /// Supply the warehouse catalog (`Requires::Catalog` rules then
    /// evaluate).
    pub fn with_catalog(mut self, catalog: Option<&'a Catalog>) -> Self {
        self.catalog = catalog;
        self
    }

    /// Pin the adapter context explicitly (the TUI's `--sql-dialect`
    /// setting when it carries no SQL resolver — `Some` overrides the
    /// `new`-resolved/`with_sql`-adopted value, `None` leaves it).
    pub fn with_dialect(mut self, dialect: Option<SqlDialect>) -> Self {
        if let Some(d) = dialect {
            self.dialect = d;
        }
        self
    }
}

impl std::fmt::Debug for LintInput<'_> {
    // `SqlParseResolver` holds interior-mutability memos with no Debug —
    // presence is all a lint-input Debug needs to show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LintInput")
            .field("dag", &self.dag)
            .field("config", &self.config)
            .field("sql", &self.sql.is_some())
            .field("catalog", &self.catalog.is_some())
            .field("dialect", &self.dialect)
            .finish()
    }
}

/// One enabled rule [`run_rules`] could not evaluate because its declared
/// [`Requires`] input was absent — skipping is informational, never a
/// failure (the TUI runs graph-only lint by design and gets the same
/// findings as before; the CLI prints `SKIP` rows so the skip is visible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SkippedRule {
    /// The skipped rule.
    pub rule: LintRule,
    /// The unmet input requirement.
    pub missing: Requires,
    /// Why it skipped — the same "graph + X" phrasing `--explain` prints.
    pub reason: &'static str,
}

/// What [`run_rules`] produced: every counted [`Finding`], the findings
/// suppression dropped, plus the rules that didn't run because their
/// required input was missing (disabled rules appear in NO list — off is
/// not "skipped").
#[derive(Debug, Default)]
pub struct LintOutcome {
    /// Every finding the enabled, runnable rules produced that no
    /// suppression dropped, in [`LintRule::ALL`]-then-sorted-uid order.
    pub findings: Vec<Finding>,
    /// The findings a `lint.rules.<id>.exclude` spec or a node's
    /// `meta.dbtl.ignore` suppressed — same order as `findings`, each
    /// carrying its [`Suppression`] (mechanism + recorded reason). They
    /// never gate and never join `findings`; surfaces report them through
    /// the summary tally or `--show-suppressed`.
    pub suppressed: Vec<Finding>,
    /// Enabled rules whose `requires` weren't satisfied, in
    /// [`LintRule::ALL`] order.
    pub skipped: Vec<SkippedRule>,
    /// Models a [`Requires::Sql`] rule could not judge because their SQL
    /// exists (raw or compiled) but wouldn't parse/select — each uid once
    /// even when several SQL rules hit the same failure. Sorted by the
    /// `BTreeSet`; `forbidden_patterns` (a raw-text rule) never
    /// contributes, and neither do models that simply HAVE no SQL.
    pub unparsed: BTreeSet<String>,
}

/// Evaluate the enabled rules over the input's dag and return every
/// [`Finding`] plus the rules skipped for missing inputs: pure and
/// deterministic (rules in [`LintRule::ALL`] order, nodes scanned in sorted
/// `unique_id` order, `related` lists sorted).
///
/// Only ROOT-PROJECT nodes can be flagged — under `--include-packages` the
/// installed-package nodes stay dependency context (a package node's
/// neighbours still count toward a finding's fanout/join predicates, but the
/// flagged `node_uid` is always an in-scope node).
pub fn run_rules(input: LintInput<'_>) -> LintOutcome {
    let LintInput {
        dag,
        config,
        sql,
        catalog,
        dialect,
    } = input;
    // Resolve the owning package ONCE — `is_root_project` re-resolves per call.
    let primary = dag.primary_package();
    let in_scope = |n: &NodeInfo| primary.as_deref().is_none_or(|p| in_package(n, p));
    let mut uids: Vec<&String> = dag.nodes().keys().collect();
    uids.sort();
    // One call-site extraction per node, shared by every graph rule.
    let sites = CallSiteResolver::new(dag);
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    // Models whose SQL exists but wouldn't parse for a `Requires::Sql`
    // rule — one uid per model regardless of how many SQL rules asked.
    let mut unparsed = BTreeSet::new();
    for rule in LintRule::ALL {
        if !config.enabled(rule) {
            continue;
        }
        // Requirement-aware dispatch: a rule only runs when its declared
        // input is present — a missing one records a skip (and the rule's
        // implementation never runs, so the resolver's memo stays empty when
        // no SQL rule can evaluate).
        let missing = match rule.info().requires {
            Requires::Sql if sql.is_none() => Some(Requires::Sql),
            Requires::Catalog if catalog.is_none() => Some(Requires::Catalog),
            _ => None,
        };
        if let Some(m) = missing {
            skipped.push(SkippedRule {
                rule,
                missing: m,
                reason: match m {
                    Requires::Sql => "requires graph + sql input",
                    Requires::Catalog => "requires graph + catalog input",
                    Requires::Graph => "requires graph input",
                },
            });
            continue;
        }
        match rule {
            LintRule::DirectJoinToSource => {
                modeling::direct_join_to_source(dag, &uids, &in_scope, config, &sites, &mut out)
            }
            LintRule::MultipleSourcesJoined => {
                modeling::multiple_sources_joined(dag, &uids, &in_scope, &sites, &mut out)
            }
            LintRule::SourceFanout => {
                modeling::source_fanout(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::ModelFanout => {
                modeling::model_fanout(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::RootModels => modeling::root_models(dag, &uids, &in_scope, &sites, &mut out),
            LintRule::RejoiningUpstream => {
                modeling::rejoining_upstream(dag, &uids, &in_scope, config, &sites, &mut out)
            }
            LintRule::MartsDependOnSource => {
                modeling::marts_depend_on_source(dag, &uids, &in_scope, config, &sites, &mut out)
            }
            LintRule::MissingPrimaryKeyTests => {
                testing::missing_primary_key_tests(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::PublicModelsWithoutContract => {
                governance::public_models_without_contract(dag, &uids, &in_scope, &mut out)
            }
            LintRule::UndocumentedModels => {
                documentation::undocumented_models(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::NamingConventions => {
                structure::naming_conventions(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::LayerViolation => modeling::layer_violation(dag, &sites, &mut out),
            LintRule::UnusedSources => modeling::unused_sources(dag, &uids, &in_scope, &mut out),
            LintRule::DuplicateSources => {
                modeling::duplicate_sources(dag, &uids, &in_scope, &mut out)
            }
            LintRule::StagingDependsOnStaging => modeling::staging_depends_on_staging(
                dag, &uids, &in_scope, config, &sites, &mut out,
            ),
            LintRule::TooManyJoins => {
                modeling::too_many_joins(dag, &uids, &in_scope, config, &sites, &mut out)
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
                documentation::undocumented_columns(dag, &uids, &in_scope, config, &mut out)
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
                modeling::incremental_without_unique_key(dag, &uids, &in_scope, &sites, &mut out)
            }
            LintRule::SourcesWithoutFreshness => {
                testing::sources_without_freshness(dag, &uids, &in_scope, &mut out)
            }
            LintRule::SelectStar => sql::select_star(
                dag,
                &uids,
                &in_scope,
                config,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
            LintRule::HardCodedReferences => sql::hard_coded_references(
                dag,
                &uids,
                &in_scope,
                config,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
            LintRule::CartesianJoin => sql::cartesian_join(
                dag,
                &uids,
                &in_scope,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
            LintRule::UnknownColumnReference => sql::unknown_column_reference(
                dag,
                &uids,
                &in_scope,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
            LintRule::ForbiddenPatterns => {
                sql::forbidden_patterns(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::WarehouseTypeDrift => warehouse::warehouse_type_drift(
                dag,
                &uids,
                &in_scope,
                catalog.expect("gated"),
                dialect,
                &mut out,
            ),
            LintRule::ColumnsMissingInWarehouse => warehouse::columns_missing_in_warehouse(
                dag,
                &uids,
                &in_scope,
                catalog.expect("gated"),
                &mut out,
            ),
            LintRule::UnusedModels => {
                modeling::unused_models(dag, &uids, &in_scope, config, &mut out)
            }
            LintRule::ReferencesDeprecatedModel => {
                governance::references_deprecated_model(dag, &uids, &in_scope, &sites, &mut out)
            }
            LintRule::UndeclaredWarehouseColumns => warehouse::undeclared_warehouse_columns(
                dag,
                &uids,
                &in_scope,
                catalog.expect("gated"),
                &mut out,
            ),
            LintRule::TrailingSemicolon => {
                sql::trailing_semicolon(dag, &uids, &in_scope, dialect, &mut out)
            }
            LintRule::DuplicateOutputColumns => sql::duplicate_output_columns(
                dag,
                &uids,
                &in_scope,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
            LintRule::UnusedCte => sql::unused_cte(
                dag,
                &uids,
                &in_scope,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
            LintRule::OrderByWithoutLimit => sql::order_by_without_limit(
                dag,
                &uids,
                &in_scope,
                sql.expect("gated"),
                &mut unparsed,
                &mut out,
            ),
        }
    }
    // The envelope fields are filled in ONE place — the severity the config
    // resolved for the rule, and the node's project file. Rule functions
    // produce bare findings (`Finding::new`) so the policy stays here.
    for f in &mut out {
        envelope(dag, config, f);
    }
    let suppressed = suppress(dag, config, &mut out);
    LintOutcome {
        findings: out,
        suppressed,
        skipped,
        unparsed,
    }
}

/// The one place the envelope fields fill: `severity` (the config's
/// resolved value) and `location.file` (the node's project file).
/// YAML-declared-attribute findings (see [`YAML_ATTRIBUTE_RULE_IDS`])
/// point at the declaring `.yml` when the loaders recorded one — a
/// missing column description lives in `schema.yml`, not the model's
/// `.sql`. Graph/SQL-level findings keep the original file.
///
/// A position a rule attached (`with_span`) survives ONLY while its
/// `file` agrees with the one the envelope resolves — when it disagrees
/// (the rule pointed at a `.sql` offset but the finding's file became a
/// `.yml`, or vice versa) the position and its labels are dropped: a
/// position naming a different file than `location.file` would lie.
/// `help` is never positional, so it survives either way.
///
/// The same reasoning clears `label`/`labels` when the finding has NO
/// location and the envelope fills a bare `file`: labels annotate a
/// position, and the position this location gains later (a `.yml` span
/// from `fill_finding_lines`) is not the one they were written for.
pub(super) fn envelope(dag: &Dag, config: &LintConfig, f: &mut Finding) {
    f.severity = config
        .severity(f.rule)
        .unwrap_or_else(|| f.rule.info().default_severity);
    let file = dag.detail(&f.node_uid).and_then(|d| {
        if crate::lint::YAML_ATTRIBUTE_RULE_IDS.contains(&f.rule.name()) {
            d.patch_path
                .clone()
                .or_else(|| d.original_file_path.clone())
        } else {
            d.original_file_path.clone()
        }
    });
    match (f.location.take(), file) {
        (Some(loc), Some(file)) => {
            f.location = Some(if loc.file == file {
                loc
            } else {
                f.label = None;
                f.labels.clear();
                Location::new(file)
            });
        }
        // The rule named a file the dag doesn't know — keep its claim.
        (loc @ Some(_), None) => f.location = loc,
        (None, file) => {
            f.label = None;
            f.labels.clear();
            f.location = file.map(Location::new);
        }
    }
}

/// One evaluated `exclude` spec — the uids its selector matched and the
/// reason it records, kept in declared order so the first match wins.
type ExcludedSet = (BTreeSet<String>, Option<String>);

/// Split findings suppressed by `lint.rules.<id>.exclude` selectors or by
/// node `meta: {dbtl: {ignore: ...}}` out of `out` — findings are removed
/// whole, never reordered, and each lands in the returned vector carrying
/// its [`Suppression`] record. Both mechanisms match on the finding's
/// `node_uid` — the CHILD side for edge rules — and a node-local `meta`
/// suppression wins the `by`/`reason` attribution over a config `exclude`
/// spec (the more specific scope records); inside `exclude` the first
/// matching spec in declared order records.
fn suppress(dag: &Dag, config: &LintConfig, out: &mut Vec<Finding>) -> Vec<Finding> {
    // Each rule's exclude spec evaluates ONCE into a uid set (the strings
    // are a union — `exclude: [a, b]` drops nodes matching a OR b) — kept
    // in declared order so the first matching spec's `reason` wins.
    let excluded: BTreeMap<LintRule, Vec<ExcludedSet>> = LintRule::ALL
        .iter()
        .filter_map(|rule| {
            let specs = config.excludes(*rule);
            if specs.is_empty() {
                None
            } else {
                let mut sets = Vec::with_capacity(specs.len());
                for spec in specs {
                    sets.push((select(dag, None, &spec.select), spec.reason.clone()));
                }
                Some((*rule, sets))
            }
        })
        .collect();
    let mut suppressed = Vec::new();
    let mut kept = Vec::with_capacity(out.len());
    for mut f in std::mem::take(out) {
        let suppression = meta_suppression(dag, &f.node_uid, f.rule)
            .map(|reason| Suppression {
                by: SuppressionSource::Meta,
                reason,
            })
            .or_else(|| {
                excluded.get(&f.rule).and_then(|sets| {
                    sets.iter()
                        .find(|(set, _)| set.contains(&f.node_uid))
                        .map(|(_, reason)| Suppression {
                            by: SuppressionSource::Exclude,
                            reason: reason.clone(),
                        })
                })
            });
        match suppression {
            Some(s) => {
                f.suppression = Some(s);
                suppressed.push(f);
            }
            None => kept.push(f),
        }
    }
    *out = kept;
    suppressed
}

/// The `meta.dbtl` object's `ignore` member — `Absent` (no `ignore` key;
/// other `meta.dbtl` keys are the project's own metadata), `"all"`, a list
/// of rule ids, a `{<rule id>|all: <reason>}` map, or a malformed value.
/// [`meta_suppression`] and [`meta_dbtl_warnings`] read through this one
/// classification so a value can never suppress by accident and still go
/// unreported.
enum MetaIgnore {
    Absent,
    All,
    List(Vec<String>),
    /// The reason map: rule id (or `all`) → the recorded reason (`~` = no
    /// reason of its own — `meta.dbtl.reason` still applies).
    Map(serde_json::Map<String, serde_json::Value>),
    Malformed,
}

/// Classify `meta.dbtl`'s `ignore` value: the string `"all"`, a list of id
/// strings, a `{<rule id>|all: <reason>|~}` map, or `Malformed` — any other
/// scalar, a non-string list item, a map value that is neither a string
/// nor `null`, any other shape.
fn meta_ignore(dbtl: &serde_json::Value) -> MetaIgnore {
    let Some(ignore) = dbtl.get("ignore") else {
        return MetaIgnore::Absent;
    };
    match ignore {
        serde_json::Value::String(s) if s == "all" => MetaIgnore::All,
        serde_json::Value::Array(ids) => {
            let mut list = Vec::with_capacity(ids.len());
            for id in ids {
                match id.as_str() {
                    Some(s) => list.push(s.to_string()),
                    None => return MetaIgnore::Malformed,
                }
            }
            MetaIgnore::List(list)
        }
        // An empty map suppresses nothing — classify it as malformed so
        // `meta_dbtl_warnings` still says the intended suppression does
        // nothing, like every other unrecognized shape.
        serde_json::Value::Object(entries) if entries.is_empty() => MetaIgnore::Malformed,
        serde_json::Value::Object(entries) => {
            for value in entries.values() {
                if !matches!(
                    value,
                    serde_json::Value::String(_) | serde_json::Value::Null
                ) {
                    return MetaIgnore::Malformed;
                }
            }
            MetaIgnore::Map(entries.clone())
        }
        _ => MetaIgnore::Malformed,
    }
}

/// Node `node_uid`'s `meta.dbtl` block, classified: the `ignore` shape plus
/// the `reason` string every suppression on the node falls back to. The
/// `dbtl` entry is a JSON object (stringified by the `Dag`'s meta
/// normalization) — a value that fails to re-parse reads as no `dbtl`
/// block at all, matching [`meta_dbtl_warnings`].
fn meta_dbtl(dag: &Dag, node_uid: &str) -> Option<(MetaIgnore, Option<String>)> {
    let raw = dag.detail(node_uid)?.meta.get("dbtl")?;
    let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let reason = v.get("reason").and_then(|r| match r {
        // An empty reason reads as none — a bare `suppressed:` marker with
        // a dangling dash is worse than no reason at all.
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        // A scalar `reason:` stringifies like every other meta value.
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(r.to_string()),
        _ => None,
    });
    Some((meta_ignore(&v), reason))
}

/// Does node `node_uid`'s `meta.dbtl.ignore` suppress `rule` — and with
/// what recorded reason? `{"ignore": ["rule_id", ...]}` suppresses the
/// listed rules, `{"ignore": "all"}` suppresses every rule, and
/// `{"ignore": {"rule_id"|"all": <reason>}}` suppresses with a per-key
/// reason; `meta.dbtl.reason` is the fallback for the list/`"all"` forms
/// and `~` map values. A malformed value suppresses nothing — suppression
/// is opt-in, never accidental — and the loader already warned once per
/// node when the manifest built ([`meta_dbtl_warnings`]), so this
/// per-finding check stays silent.
fn meta_suppression(dag: &Dag, node_uid: &str, rule: LintRule) -> Option<Option<String>> {
    let (ignore, fallback) = meta_dbtl(dag, node_uid)?;
    match ignore {
        MetaIgnore::All => Some(fallback),
        MetaIgnore::List(ids) => ids.iter().any(|id| id == rule.name()).then_some(fallback),
        MetaIgnore::Map(entries) => {
            // A rule-named key wins over `all` — the specific record is the
            // more precise reason. `~` values fall back to `meta.dbtl.reason`.
            let entry = entries.get(rule.name()).or_else(|| entries.get("all"))?;
            Some(
                entry
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or(fallback),
            )
        }
        MetaIgnore::Absent | MetaIgnore::Malformed => None,
    }
}

/// One load warning per node/source whose `meta.dbtl.ignore` is malformed
/// — not `"all"`, not a list of rule ids, not a `{rule id|all: reason}`
/// map. The shape means the suppression the author intended is silently
/// doing nothing (see [`meta_suppression`]), so it surfaces like any other
/// loader warning — once per node, not once per finding. Scans the raw
/// manifest so manifest and source loads warn identically.
pub(crate) fn meta_dbtl_warnings(manifest: &crate::RawManifest) -> Vec<String> {
    let mut warnings = Vec::new();
    for (uid, meta) in manifest
        .nodes
        .iter()
        .map(|(uid, node)| (uid, &node.meta))
        .chain(manifest.sources.iter().map(|(uid, src)| (uid, &src.meta)))
    {
        let Some(dbtl) = meta.get("dbtl") else {
            continue;
        };
        // `meta.dbtl` can itself be a JSON-encoded string (`dbtl:
        // '{"ignore": …}'`): the suppression reader re-parses the
        // stringified `detail.meta` value the same way, so mirror it here —
        // a string that decodes to an object is classified by its
        // contents, any other string carries no `ignore` at all.
        let reparsed;
        let dbtl = match dbtl {
            serde_json::Value::String(s) => {
                reparsed = serde_json::from_str::<serde_json::Value>(s).ok();
                match &reparsed {
                    Some(v @ serde_json::Value::Object(_)) => v,
                    _ => continue,
                }
            }
            other => other,
        };
        match meta_ignore(dbtl) {
            MetaIgnore::Malformed => warnings.push(format!(
                "{uid}: meta.dbtl.ignore must be \"all\", a list of lint rule ids, \
                 or a {{rule-id|all: reason}} map"
            )),
            MetaIgnore::List(ids) => {
                let unknown: Vec<&str> = ids
                    .iter()
                    .map(String::as_str)
                    .filter(|id| LintRule::from_name(id).is_none())
                    .collect();
                if !unknown.is_empty() {
                    warnings.push(format!(
                        "{uid}: meta.dbtl.ignore names unknown lint rule id(s): {}",
                        unknown.join(", ")
                    ));
                }
            }
            MetaIgnore::Map(entries) => {
                // `all` is a valid key in the map form — everything else
                // must name a rule id.
                let unknown: Vec<&str> = entries
                    .keys()
                    .map(String::as_str)
                    .filter(|id| *id != "all" && LintRule::from_name(id).is_none())
                    .collect();
                if !unknown.is_empty() {
                    warnings.push(format!(
                        "{uid}: meta.dbtl.ignore names unknown lint rule id(s): {}",
                        unknown.join(", ")
                    ));
                }
            }
            MetaIgnore::Absent | MetaIgnore::All => {}
        }
        // `meta.dbtl.reason` is the fallback every map-`~`/list
        // suppression reads — a map/list value drops silently in
        // `meta_dbtl`, so say so here like a malformed `ignore`.
        if let Some(r) = dbtl.get("reason") {
            if !matches!(
                r,
                serde_json::Value::String(_)
                    | serde_json::Value::Number(_)
                    | serde_json::Value::Bool(_)
                    | serde_json::Value::Null
            ) {
                warnings.push(format!(
                    "{uid}: meta.dbtl.reason must be a string or number/bool scalar"
                ));
            }
        }
    }
    warnings.sort();
    warnings
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
/// child test and as endpoints of the rejoin — "single use" counts
/// data-carrying nodes only. Endpoint
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

/// Whether `n` is a model whose SQL body a rule can read —
/// `resource_type == "model"` and not `language == "python"`. A Python
/// model's `raw_code` is Python source, so scanning it for wildcards,
/// relation names, or regex hits fabricates findings (the position pass
/// independently refuses `.py` files — this keeps it from flagging at
/// all). No `detail` entry = language unknown = treated as SQL.
pub(super) fn is_sql_model(dag: &Dag, n: &NodeInfo) -> bool {
    n.resource_type == "model"
        && dag
            .detail(&n.unique_id)
            .is_none_or(|d| d.language.as_deref() != Some("python"))
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
/// it falls through to 'other', not to an implicit layer).
pub(super) fn lint_model_type_by_dir<'a>(n: &NodeInfo, config: &'a LintConfig) -> Option<&'a str> {
    let dir = first_dir(n)?;
    config
        .naming_prefixes
        .get_key_value(dir)
        .map(|(layer, _)| layer.as_str())
}

/// Resolve a model's lint layer — the two stages above in order, then
/// `"other"`:
///
/// 1. [`lint_model_type_by_prefix`] — a [`naming_prefixes`](LintConfig::naming_prefixes)
///    layer whose configured prefix the model name starts with;
/// 2. [`lint_model_type_by_dir`] — the [`first_dir`] folder, when it is
///    itself a configured layer;
/// 3. `"other"`, the catch-all for unclassified models.
///
/// Returning the CONFIG KEY (not the dir) keeps the result comparable across
/// consumers: `marts_depend_on_source` matches it against `marts` /
/// `intermediate`, and `naming_conventions` looks the expected prefixes back
/// up in the same map. The `"other"` type only carries prefixes when the
/// config defines an `other:` layer (enforcing prefixes on unclassified
/// models is opt-in).
pub(super) fn lint_model_type<'a>(n: &NodeInfo, config: &'a LintConfig) -> &'a str {
    lint_model_type_by_prefix(n, config)
        .or_else(|| lint_model_type_by_dir(n, config))
        .unwrap_or("other")
}
