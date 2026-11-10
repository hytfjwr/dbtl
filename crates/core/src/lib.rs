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
pub mod impact;
pub mod layer;
pub mod lint;
pub mod manifest;
pub mod mermaid;
pub mod run_results;
pub mod select;
pub mod source;

pub use catalog::{default_catalog_path, Catalog, CatalogColumn, CatalogNode, CatalogStat};
pub use cll::{
    closure_report, normalize_column, resolution_sql, ClosureDirection, ColumnEdge, ColumnEdgeKind,
    ColumnLineage, ColumnRef, ColumnReport, ColumnResolver, ColumnRow, Confidence,
    NameMatchResolver, SqlDialect, SqlParseResolver,
};
pub use compiled::{default_compiled_dir, default_target_dir, CompiledSql};
pub use dag::{
    coverage_gap, has_pk_guarantee, in_package, is_exposure, load_dag, load_dag_with_warnings,
    unhandled_resource_types, ColumnInfo, Dag, Edge, ExposureInfo, MacroInfo, NodeDetail, NodeInfo,
    PackageScope, ResourceTypeSupport, Subgraph, TestInfo, UnitTestShape,
};
pub use diff::{compute_diff, DagDiff, DiffStatus};
pub use impact::{
    column_coverage_counts, coverage_counts, diff_report, exposure_note, layer_violation_edges,
    pr_impact, pr_impact_markdown, DiffReport, PrImpact, PR_HUB_MIN_DOWNSTREAM,
};
pub use layer::{first_dir, layer_rank, LAYER_ORDER};
pub use lint::{
    lint_config_candidates, lint_config_path, run_rules, Finding, LintConfig, LintRule,
    LINT_CONFIG_NAME,
};
pub use manifest::{
    load_manifest, RawConfig, RawDependsOn, RawExposure, RawExposureOwner, RawManifest,
    RawMetadata, RawNode, RawSource, RawTestMetadata, RawUnitTest, RawUnitTestIo,
};
pub use mermaid::{
    mermaid_ids, mermaid_label, node_kind, subgraph_mermaid, subgraph_mermaid_linked,
};
pub use run_results::{default_run_results_path, RunResult, RunResults, RunStatus};
pub use select::select;
pub use source::{
    load_dag_from_source, load_dag_from_source_with_warnings, manifest_from_source,
    manifest_from_source_with_warnings,
};
