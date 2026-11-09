//! `run_results.json` ingestion: the runtime side-map the `RunStatus`/`RunTime`
//! lenses and the runtime-weighted critical path read. A run_results file is a
//! *partial* artifact — it covers whatever the last `dbt run`/`build`/`test`
//! executed, not the whole graph — so [`RunResults::covered_in`] answers "how
//! many of THIS Dag's nodes have results" rather than assuming full coverage.
//!
//! Not part of `Dag`: the manifest describes structure; run results describe
//! one execution. The two are loaded separately and joined on `unique_id`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// One node's outcome from a single dbt invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// The execution status (`error`/`fail`/`warn`/`skipped`/`success`/`pass`).
    pub status: RunStatus,
    /// Wall-clock seconds the node took (`execution_time` in the artifact;
    /// `None` for skipped nodes and odd payloads).
    pub execution_time: Option<f64>,
}

/// The dbt run-status vocabulary. `success` and `pass` (test runs) both mean
/// "went fine"; anything outside the enum is kept verbatim in `Other`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    /// Ran fine (`success`/`pass`).
    Success,
    /// Raised (`error`).
    Error,
    /// Failed its assertion (`fail`, test nodes).
    Fail,
    /// Passed with a warning (`warn`).
    Warn,
    /// Not executed (`skipped`).
    Skipped,
    /// A status dbtl doesn't know — kept for completeness.
    Other(String),
}

impl RunStatus {
    /// Map the artifact's raw status string onto the enum.
    fn parse(s: &str) -> RunStatus {
        match s {
            "success" | "pass" => RunStatus::Success,
            "error" => RunStatus::Error,
            "fail" => RunStatus::Fail,
            "warn" => RunStatus::Warn,
            "skipped" => RunStatus::Skipped,
            other => RunStatus::Other(other.to_string()),
        }
    }

    /// Whether the run ended badly (the RunStatus lens's red class).
    pub fn is_failure(&self) -> bool {
        matches!(self, RunStatus::Error | RunStatus::Fail)
    }
}

// ---- wire types (serde) -----------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawRunMeta {
    #[serde(default)]
    generated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRunEntry {
    unique_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    execution_time: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawRunResults {
    #[serde(default)]
    metadata: Option<RawRunMeta>,
    #[serde(default)]
    results: Vec<RawRunEntry>,
}

/// A loaded `run_results.json`: `unique_id` → outcome, plus the artifact's
/// `generated_at` timestamp (ISO-8601 string, shown verbatim so users can see
/// how stale the results are). Keyed map → deterministic lookups; the file's
/// own ordering is irrelevant to every consumer.
#[derive(Debug, Clone, Default)]
pub struct RunResults {
    /// `metadata.generated_at` from the artifact, verbatim.
    pub generated_at: Option<String>,
    results: BTreeMap<String, RunResult>,
}

impl RunResults {
    /// Read and parse a `run_results.json`. Malformed JSON is an `Err`; a file
    /// with an empty/absent `results` array is a valid, empty `RunResults`.
    pub fn load(path: &Path) -> Result<RunResults> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read run_results at {}", path.display()))?;
        RunResults::from_json(&text)
            .with_context(|| format!("cannot parse run_results at {}", path.display()))
    }

    /// Parse the JSON payload itself (split out so tests need no fixture file).
    pub fn from_json(text: &str) -> Result<RunResults> {
        let raw: RawRunResults = serde_json::from_str(text)?;
        let results = raw
            .results
            .into_iter()
            .map(|e| {
                (
                    e.unique_id,
                    RunResult {
                        status: RunStatus::parse(&e.status),
                        execution_time: e.execution_time,
                    },
                )
            })
            .collect();
        Ok(RunResults {
            generated_at: raw.metadata.and_then(|m| m.generated_at),
            results,
        })
    }

    /// The result for a `unique_id`, or `None` when the run didn't cover it.
    pub fn get(&self, uid: &str) -> Option<&RunResult> {
        self.results.get(uid)
    }

    /// The execution status for a `unique_id`.
    pub fn status(&self, uid: &str) -> Option<&RunStatus> {
        self.results.get(uid).map(|r| &r.status)
    }

    /// The execution time (seconds) for a `unique_id`.
    pub fn execution_time(&self, uid: &str) -> Option<f64> {
        self.results.get(uid).and_then(|r| r.execution_time)
    }

    /// Result count in the artifact (may exceed the Dag's size — a partial run
    /// can report nodes a source-parsed Dag never had).
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// `true` when the artifact carried no results.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// How many of `dag`'s nodes the run covered — the "what does this file
    /// even describe" number the status bar shows, since a partial
    /// `dbt run --select …` writes results for a subset only.
    pub fn covered_in(&self, dag: &crate::Dag) -> usize {
        self.results
            .keys()
            .filter(|uid| dag.get(uid).is_some())
            .count()
    }
}

/// Where the sibling `run_results.json` for a data source lives: next to a
/// compiled manifest (`target/run_results.json`), or under a project dir's
/// `target/` in source mode.
pub fn default_run_results_path(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<std::path::PathBuf> {
    if let Some(m) = manifest_path {
        m.parent().map(|dir| dir.join("run_results.json"))
    } else {
        project_dir.map(|dir| dir.join("target").join("run_results.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "metadata": {"generated_at": "2024-05-01T12:34:56.789Z", "dbt_version": "1.8.0"},
        "elapsed_time": 12.5,
        "results": [
            {"unique_id": "model.p.ok", "status": "success", "execution_time": 1.25},
            {"unique_id": "model.p.bad", "status": "error", "execution_time": 30.5},
            {"unique_id": "test.p.t1", "status": "fail", "execution_time": 0.5},
            {"unique_id": "test.p.t2", "status": "warn", "execution_time": 0.4},
            {"unique_id": "model.p.lazy", "status": "skipped", "execution_time": null},
            {"unique_id": "model.p.new_kind", "status": "partial"}
        ]
    }"#;

    #[test]
    fn parses_statuses_times_and_generated_at() {
        let rr = RunResults::from_json(SAMPLE).expect("valid run_results");
        assert_eq!(rr.generated_at.as_deref(), Some("2024-05-01T12:34:56.789Z"));
        assert_eq!(rr.len(), 6);
        assert_eq!(rr.status("model.p.ok"), Some(&RunStatus::Success));
        assert_eq!(rr.status("model.p.bad"), Some(&RunStatus::Error));
        assert!(rr.status("model.p.bad").unwrap().is_failure());
        assert!(rr.status("test.p.t1").unwrap().is_failure());
        assert_eq!(rr.status("test.p.t2"), Some(&RunStatus::Warn));
        assert_eq!(rr.status("model.p.lazy"), Some(&RunStatus::Skipped));
        assert_eq!(
            rr.status("model.p.new_kind"),
            Some(&RunStatus::Other("partial".to_string()))
        );
        assert_eq!(rr.execution_time("model.p.bad"), Some(30.5));
        assert_eq!(rr.execution_time("model.p.lazy"), None);
        assert!(rr.get("model.p.absent").is_none(), "uncovered node → None");
    }

    #[test]
    fn empty_results_is_a_valid_empty_map() {
        let rr = RunResults::from_json(r#"{"results": []}"#).expect("empty ok");
        assert!(rr.is_empty());
        assert!(rr.generated_at.is_none());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(RunResults::from_json("{ not json").is_err());
    }
}
