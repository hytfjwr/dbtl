//! dbtl-core — the dbt domain model, with zero terminal dependencies.
//!
//! A `manifest.json` ([`manifest`] + [`load_dag`]) or a dbt project tree
//! ([`source`], which synthesizes a [`RawManifest`]) becomes a pruned [`Dag`]
//! ([`dag`]). [`diff`] compares two `Dag`s, [`layer`] is the shared
//! staging/intermediate/marts/utilities taxonomy, and [`mermaid`] is the pure
//! `Subgraph`→Mermaid text core shared by the TUI yank and `dbtl-docs`.

pub mod catalog;
pub mod cll;
pub mod compiled;
pub mod dag;
pub mod diff;
pub mod gates;
pub mod impact;
pub mod layer;
pub mod lint;
pub mod manifest;
pub mod mermaid;
pub mod project_config;
pub mod run_results;
pub mod select;
pub mod source;

pub use catalog::{
    default_catalog_path, resolve_catalog_path, types_equivalent, Catalog, CatalogColumn,
    CatalogNode, CatalogStat, TypeDrift,
};
pub use cll::{
    closure_report, normalize_column, resolution_sql, ClosureDirection, ColumnEdge, ColumnEdgeKind,
    ColumnLineage, ColumnRef, ColumnReport, ColumnResolver, ColumnRow, Confidence, MarkedRelations,
    NameMatchResolver, SqlDialect, SqlFacts, SqlParseResolver, SqlPositions,
};
pub use compiled::{default_compiled_dir, default_target_dir, CompiledOrigin, CompiledSql};
pub use dag::{
    coverage_gap, description_blank, has_pk_guarantee, in_package, is_exposure, load_dag,
    load_dag_with_warnings, unhandled_resource_types, ColumnInfo, ConstraintInfo, Dag, Edge,
    ExposureInfo, FreshnessThreshold, MacroInfo, NodeDetail, NodeInfo, PackageScope,
    PrunedAdjacent, ResourceTypeSupport, Subgraph, TestInfo, UnitTestShape,
};
pub use diff::{
    compute_diff, compute_diff_with, ColumnChange, ColumnChangeClass, ColumnChangeKind, DagDiff,
    DiffStatus,
};
pub use gates::{CheckConfig, CheckGate, GateParamKind, GateParamSpec, GateSetting};
pub use impact::{
    column_coverage_counts, column_doc_coverage_counts, coverage_counts, diff_report,
    doc_coverage_counts, exposure_note, layer_violation_edges, model_documented, pr_impact,
    pr_impact_markdown, DiffReport, ImpactOpts, PrImpact, PR_HUB_MIN_DOWNSTREAM,
};
pub use layer::{first_dir, layer_rank, LAYER_ORDER, VIOLATION_LAYERS};
pub use lint::{
    byte_span_position, display_name, display_names, fill_finding_lines, name_list, removed_rule,
    removed_rule_error, run_rules, yaml_name_line, yaml_name_span, Baseline, Category, Finding,
    Label, LintConfig, LintInput, LintOutcome, LintRule, Location, ParamKind, ParamSpec,
    RemovedRule, Requires, RuleInfo, Severity, SkippedRule, BASELINE_SCHEMA, DEFAULT_BASELINE_NAME,
    LINT_RULE_DOCS_BASE, REGISTRY, REMOVED_RULES, YAML_ATTRIBUTE_RULE_IDS,
};
pub use manifest::{
    load_manifest, RawConfig, RawConstraint, RawContract, RawDependsOn, RawDoc, RawExposure,
    RawExposureOwner, RawFreshness, RawFreshnessThreshold, RawManifest, RawMetadata, RawNode,
    RawSemanticResource, RawSource, RawTestMetadata, RawUnitTest, RawUnitTestIo,
};
pub use mermaid::{
    mermaid_ids, mermaid_label, node_kind, subgraph_mermaid, subgraph_mermaid_linked,
};
pub use project_config::{
    project_config_candidates, project_config_path, DocsConfig, FailOn, LineageDepth,
    ProjectConfig, PROJECT_CONFIG_NAME,
};
#[cfg(feature = "schema")]
pub use project_config::{project_config_schema, severity_setting_schema};
pub use run_results::{default_run_results_path, RunResult, RunResults, RunStatus};
pub use select::{select, selector_uses_state, validate_selector};
pub use source::{
    load_dag_from_source, load_dag_from_source_with_warnings, manifest_from_source,
    manifest_from_source_with_warnings,
};
