//! The post-diff readouts shared by the `D` modal, `dbtl impact`, and `dbtl
//! check`'s impact gates — plus the coverage/counts family the status `cov`
//! segment, `t` lens, stats dashboard, `dbtl docs` summaries, and `dbtl
//! check` gates all read. Every listing resolves NAMES (a `RawNode.name`,
//! an exposure's `name`, the uid itself when it isn't in the dag — with an
//! empty kind slot) and is sorted —
//! one diff yields one stable report, so re-opening the same diff in the
//! TUI and re-running `dbtl impact` print the same bytes.
//!
//! Split by responsibility: `report` holds the shared payload model and its
//! assembly, `pr` the PR-impact analysis and the shared primitives it
//! composes, `coverage` the root-project coverage/description counters, and
//! `markdown` the readout's byte-stable formatter. The `pub use` block keeps
//! every public name on `crate::impact` — callers' `crate::X` paths are
//! unchanged.

mod coverage;
mod markdown;
mod pr;
mod report;
#[cfg(test)]
mod tests;

pub use coverage::{
    column_coverage_counts, column_doc_coverage_counts, coverage_counts, doc_coverage_counts,
    model_documented,
};
pub use markdown::pr_impact_markdown;
pub use pr::{exposure_note, layer_violation_edges, pr_impact};
pub use report::{diff_report, DiffReport, ImpactOpts, PrImpact, PR_HUB_MIN_DOWNSTREAM};
