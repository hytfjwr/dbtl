//! The `dbtl check` gate registry — the ONE table of the CI gates
//! `dbtl check` can evaluate: each gate's stable id (the `.dbtl.yml`
//! `check.gates` key AND the report's machine name), its report title, its
//! default severity, whether it needs a `--diff` baseline, and the
//! parameters its config entry accepts. The gate evaluation in the CLI and
//! the `.dbtl.yml` validation both read this table, so a gate can never
//! exist in one place but not the other, and an accepted parameter name
//! can't drift from the gate that consumes it.

use crate::lint::Severity;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One named `dbtl check` gate — declared in report order so
/// [`CheckGate::ALL`] iterates the order gate rows print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckGate {
    /// Root-project models/snapshots/seeds with >= 1 test (`--coverage-min`).
    Coverage,
    /// Declared columns the schema test graph covers (`column_coverage_counts`).
    ColumnCoverage,
    /// Root-project models carrying a `description` (`doc_coverage_counts`).
    DocCoverage,
    /// Edges that violate the layer taxonomy in the baseline→current diff.
    NewLayerViolations,
    /// Changed nodes without any tests.
    UntestedChanges,
    /// Removed/renamed columns still referenced downstream (`--no-breaking`).
    BreakingColumns,
    /// Changed nodes at the hub threshold — the same list the PR impact
    /// pack prints, at the same `min_downstream`.
    ChangedHubs,
    /// Exposures whose upstream set intersects the blast radius.
    AffectedExposures,
    /// The blast radius itself: `affected_models.len()` vs `max`.
    BlastRadius,
    /// The generated-docs directory is out of date (`--docs-dir` /
    /// `check.gates.docs_drift.dir`) — the same comparison `docs --check`
    /// runs.
    DocsDrift,
}

impl CheckGate {
    /// Every gate, in report order (the coverage family first, then the
    /// diff gates, then docs drift).
    pub const ALL: [CheckGate; 10] = [
        CheckGate::Coverage,
        CheckGate::ColumnCoverage,
        CheckGate::DocCoverage,
        CheckGate::NewLayerViolations,
        CheckGate::UntestedChanges,
        CheckGate::BreakingColumns,
        CheckGate::ChangedHubs,
        CheckGate::AffectedExposures,
        CheckGate::BlastRadius,
        CheckGate::DocsDrift,
    ];

    /// `.dbtl.yml`'s `check.gates.<id>` key → gate (`None` = unknown id).
    pub fn from_name(name: &str) -> Option<CheckGate> {
        Self::ALL.iter().find(|g| g.id() == name).copied()
    }

    /// Every gate id, comma-joined — the valid-set list the unknown-gate
    /// errors print (`.dbtl.yml`'s parser and the `--gate` flag parser
    /// share it, so the two front ends can't list different gates).
    pub fn valid_ids() -> String {
        Self::ALL
            .iter()
            .map(|g| g.id())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The gate's stable id — the `check.gates` key, the report's machine
    /// name, and the `--gate` left-hand side.
    pub fn id(self) -> &'static str {
        self.info().id
    }

    /// The text/markdown row title (`coverage`, `new layer violations`).
    pub fn title(self) -> &'static str {
        self.info().title
    }

    /// The severity the gate reports at when nothing configures it —
    /// `None` = off until enabled.
    pub fn default_severity(self) -> Option<Severity> {
        self.info().default_severity
    }

    /// `true` when the gate needs a `--diff` baseline to evaluate — an
    /// explicitly-enabled one reports `skip` (never `fail`) without it.
    pub fn needs_diff(self) -> bool {
        self.info().needs_diff
    }

    /// A one-line description of what the gate checks — the config
    /// reference and the generated schema share it.
    pub fn summary(self) -> &'static str {
        self.info().summary
    }

    /// The parameters `check.gates.<id>` accepts beyond `severity`.
    pub fn params(self) -> &'static [GateParamSpec] {
        self.info().params
    }

    fn info(self) -> &'static CheckGateInfo {
        match self {
            CheckGate::Coverage => &CheckGateInfo {
                id: "coverage",
                title: "coverage",
                summary: "Root-project models/snapshots/seeds with a test below `min` percent",
                default_severity: Some(Severity::Error),
                needs_diff: false,
                params: MIN_PCT,
            },
            CheckGate::ColumnCoverage => &CheckGateInfo {
                id: "column_coverage",
                title: "column coverage",
                summary: "Declared columns covered by the schema-test graph below `min` percent",
                default_severity: None,
                needs_diff: false,
                params: MIN_PCT,
            },
            CheckGate::DocCoverage => &CheckGateInfo {
                id: "doc_coverage",
                title: "doc coverage",
                summary: "Models/snapshots/seeds with a description below `min` percent",
                default_severity: None,
                needs_diff: false,
                params: MIN_PCT,
            },
            CheckGate::NewLayerViolations => &CheckGateInfo {
                id: "new_layer_violations",
                title: "new layer violations",
                summary: "Layer-violating edges the diff introduces (needs `--diff`)",
                default_severity: Some(Severity::Error),
                needs_diff: true,
                params: NO_PARAMS,
            },
            CheckGate::UntestedChanges => &CheckGateInfo {
                id: "untested_changes",
                title: "untested changes",
                summary: "Models added or modified by the diff that have no tests (needs `--diff`)",
                default_severity: Some(Severity::Error),
                needs_diff: true,
                params: NO_PARAMS,
            },
            CheckGate::BreakingColumns => &CheckGateInfo {
                id: "breaking_columns",
                title: "breaking column changes",
                summary: "Columns the diff removes or renames that dependents still reference (needs `--diff`)",
                default_severity: None,
                needs_diff: true,
                params: NO_PARAMS,
            },
            CheckGate::ChangedHubs => &CheckGateInfo {
                id: "changed_hubs",
                title: "changed hubs",
                summary: "Diff-changed models with `min_downstream` or more dependents (needs `--diff`)",
                default_severity: Some(Severity::Warn),
                needs_diff: true,
                params: MIN_DOWNSTREAM,
            },
            CheckGate::AffectedExposures => &CheckGateInfo {
                id: "affected_exposures",
                title: "affected exposures",
                summary: "Exposures whose upstream intersects the diff's blast radius (needs `--diff`)",
                default_severity: Some(Severity::Warn),
                needs_diff: true,
                params: NO_PARAMS,
            },
            CheckGate::BlastRadius => &CheckGateInfo {
                id: "blast_radius",
                title: "blast radius",
                summary: "The diff's blast radius — affected models over `max` (needs `--diff`)",
                default_severity: None,
                needs_diff: true,
                params: MAX_COUNT,
            },
            CheckGate::DocsDrift => &CheckGateInfo {
                id: "docs_drift",
                title: "docs drift",
                summary: "The generated docs under `dir` differ from the project's current output",
                default_severity: Some(Severity::Error),
                needs_diff: false,
                params: DIR_PATH,
            },
        }
    }
}

/// The static description of one gate.
pub struct CheckGateInfo {
    /// The `check.gates` key + report machine name (`coverage`,
    /// `docs_drift`, `changed_hubs`, ...).
    pub id: &'static str,
    /// The report row title (`coverage`, `new layer violations`).
    pub title: &'static str,
    /// A one-line description of what the gate checks.
    pub summary: &'static str,
    /// `None` = the gate is off until a config entry or flag enables it.
    pub default_severity: Option<Severity>,
    /// Whether evaluation needs a `--diff` baseline.
    pub needs_diff: bool,
    /// Accepted `check.gates.<id>` parameters beyond `severity`.
    pub params: &'static [GateParamSpec],
}

/// One declared gate parameter — the gate's own row in `params`.
pub struct GateParamSpec {
    /// The `check.gates.<id>.<name>` key.
    pub name: &'static str,
    pub kind: GateParamKind,
}

/// What a gate parameter's value must look like.
#[derive(Debug, Clone, Copy)]
pub enum GateParamKind {
    /// An integer in the inclusive `[min, max]` range.
    Int { min: usize, max: usize },
    /// A directory path (resolved against `.dbtl.yml`'s directory at load).
    Path,
}

const NO_PARAMS: &[GateParamSpec] = &[];
/// `coverage`/`column_coverage`/`doc_coverage`'s percent floor.
const MIN_PCT: &[GateParamSpec] = &[GateParamSpec {
    name: "min",
    kind: GateParamKind::Int { min: 0, max: 100 },
}];
/// `changed_hubs`'s hub threshold — >= 1: a node with zero downstream
/// dependents can never be a hub.
const MIN_DOWNSTREAM: &[GateParamSpec] = &[GateParamSpec {
    name: "min_downstream",
    kind: GateParamKind::Int {
        min: 1,
        max: usize::MAX,
    },
}];
/// `blast_radius`'s affected-model ceiling.
const MAX_COUNT: &[GateParamSpec] = &[GateParamSpec {
    name: "max",
    kind: GateParamKind::Int {
        min: 0,
        max: usize::MAX,
    },
}];
/// `docs_drift`'s generated-docs directory.
const DIR_PATH: &[GateParamSpec] = &[GateParamSpec {
    name: "dir",
    kind: GateParamKind::Path,
}];

/// One gate's settings under `check.gates.<id>` (or its CLI override). The
/// struct stays flat — each gate only ever reads the slot its declared
/// params name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GateSetting {
    /// The effective severity — `None` = the gate is off (`off` in the
    /// file/flag folds here). An entry that sets only params still resolves
    /// the gate's [`CheckGate::default_severity`] at parse.
    pub severity: Option<Severity>,
    /// `min` — the percent floor for `coverage`/`column_coverage`/
    /// `doc_coverage`.
    pub min: Option<usize>,
    /// `max` — the `blast_radius` affected-model ceiling.
    pub max: Option<usize>,
    /// `min_downstream` — the `changed_hubs` hub threshold.
    pub min_downstream: Option<usize>,
    /// `dir` — `docs_drift`'s generated-docs directory.
    pub dir: Option<PathBuf>,
}

/// The resolved `check:` section: `check.lint` plus the gates the user
/// named. Gates absent from `gates` resolve to their registry defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckConfig {
    /// `check.lint` — fold the lint gates into `check` runs without
    /// `--lint`.
    pub lint: bool,
    /// The gates the user named — an entry's presence is what "explicit"
    /// means for the `needs --diff` skip row.
    pub gates: BTreeMap<CheckGate, GateSetting>,
}

impl CheckConfig {
    /// The gate's effective severity: the user's setting when named, else
    /// the registry default. `None` = the gate doesn't run.
    pub fn severity(&self, gate: CheckGate) -> Option<Severity> {
        self.gates
            .get(&gate)
            .map(|s| s.severity)
            .unwrap_or_else(|| gate.default_severity())
    }

    /// Whether the user named the gate (config entry or CLI flag) — only an
    /// explicitly-enabled diff gate reports a `skip` row without `--diff`;
    /// a default-on one just isn't listed, exactly as before.
    pub fn explicit(&self, gate: CheckGate) -> bool {
        self.gates.contains_key(&gate)
    }

    /// The `min` percent floor for the coverage gates — default 0, so an
    /// enabled gate with no configured floor can never trip (the same as
    /// `--coverage-min` never set).
    pub fn min(&self, gate: CheckGate) -> usize {
        self.gates.get(&gate).and_then(|s| s.min).unwrap_or(0)
    }

    /// `blast_radius`'s `max` — the affected-model ceiling, default 0 (any
    /// blast radius trips an enabled unconfigured gate).
    pub fn max(&self, gate: CheckGate) -> usize {
        self.gates.get(&gate).and_then(|s| s.max).unwrap_or(0)
    }

    /// `changed_hubs`'s `min_downstream` — the SAME threshold the report's
    /// `## Hubs` section lists at ([`crate::PR_HUB_MIN_DOWNSTREAM`] by
    /// default), so the gate and the pack can never disagree on what a hub
    /// is.
    pub fn min_downstream(&self) -> usize {
        self.gates
            .get(&CheckGate::ChangedHubs)
            .and_then(|s| s.min_downstream)
            .unwrap_or(crate::PR_HUB_MIN_DOWNSTREAM)
    }

    /// `docs_drift`'s configured `dir` (`--docs-dir` wins over it at the
    /// CLI merge).
    pub fn docs_dir(&self) -> Option<&Path> {
        self.gates
            .get(&CheckGate::DocsDrift)
            .and_then(|s| s.dir.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gate_id_round_trips_and_is_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for gate in CheckGate::ALL {
            assert!(seen.insert(gate.id()), "duplicate gate id {}", gate.id());
            assert_eq!(CheckGate::from_name(gate.id()), Some(gate));
        }
        assert_eq!(CheckGate::from_name("not_a_gate"), None);
    }

    #[test]
    fn unconfigured_gates_resolve_their_registry_defaults() {
        let cfg = CheckConfig::default();
        assert_eq!(cfg.severity(CheckGate::Coverage), Some(Severity::Error));
        assert_eq!(cfg.severity(CheckGate::ChangedHubs), Some(Severity::Warn));
        assert_eq!(cfg.severity(CheckGate::BreakingColumns), None);
        assert_eq!(cfg.severity(CheckGate::ColumnCoverage), None);
        assert_eq!(cfg.severity(CheckGate::BlastRadius), None);
        assert!(!cfg.explicit(CheckGate::Coverage));
        assert_eq!(cfg.min(CheckGate::Coverage), 0);
        assert_eq!(cfg.max(CheckGate::BlastRadius), 0);
        assert_eq!(cfg.min_downstream(), crate::PR_HUB_MIN_DOWNSTREAM);
    }
}
