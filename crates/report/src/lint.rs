//! Lint gate evaluation — the shared half of `dbtl check` and `dbtl lint`:
//! `run_rules` findings are classified into counted / baselined /
//! pre-existing, gated by severity, and lifted into [`GateResult`] rows (one
//! per evaluated rule plus one SKIP row per input-skipped rule, in
//! `LintRule::ALL` order). [`lint_assembly`] feeds `check --lint`'s gate
//! list; [`lint_report`] packages the whole lint run into a [`Report`] —
//! one code path, so the CLI's stdout, the uploaded report, and any
//! in-memory caller can never disagree about what the lint gates said.

use crate::{GateResult, GateStatus, LintTally, Report};
use dbtl_core::{
    Baseline, Catalog, Dag, FailOn, Finding, LintConfig, LintInput, LintRule, Severity,
    SkippedRule, SqlParseResolver,
};
use std::collections::BTreeSet;

/// Does a `severity` finding fail under `fail_on`? `warn` gates on every
/// counted finding (warnings included); `error` on errors only; `never` on
/// nothing — findings still report, the exit just stays 0.
pub fn severity_fails(severity: Severity, fail_on: FailOn) -> bool {
    match fail_on {
        FailOn::Never => false,
        FailOn::Warn => true,
        FailOn::Error => severity >= Severity::Error,
    }
}

/// The verdict for a gate that DID find violations of `severity`: `fail`
/// when `severity_fails`, else `warn` — reported, never counted.
pub fn gate_status(severity: Severity, fail_on: FailOn) -> GateStatus {
    if severity_fails(severity, fail_on) {
        GateStatus::Fail
    } else {
        GateStatus::Warn
    }
}

/// The auxiliary inputs a lint run carries — the shared lazy SQL resolver
/// and the warehouse catalog, plus the BASELINE side's resolver under
/// `--diff` (the baseline's own warehouse record is never loaded, matching
/// the diff's resolver policy — catalog rules can't compare and simply
/// don't feed `pre_existing`).
#[derive(Clone, Copy)]
pub struct LintAux<'a> {
    /// The current side's SQL resolver — `Some` feeds `LintInput::sql`.
    pub sql: Option<&'a SqlParseResolver>,
    /// The current side's catalog — `None` skips the `Requires::Catalog`
    /// rules (recorded, never a failure).
    pub catalog: Option<&'a Catalog>,
    /// The baseline side's SQL resolver under `--diff` (`None` skips SQL
    /// rules there too — their current-side findings then all count as
    /// new, the safe direction for a gate).
    pub base_sql: Option<&'a SqlParseResolver>,
}

/// The verdict for one enabled rule inside a [`LintGateOutcome`]: its effective
/// severity, which findings count, and how they were classified.
pub struct RuleOutcome {
    /// The rule.
    pub rule: LintRule,
    /// Its effective severity — uniform across its findings.
    pub severity: Severity,
    /// Indices into [`LintGateOutcome::findings`] that count toward the gate:
    /// not baselined, and new under `--diff`.
    pub counted: Vec<usize>,
    /// Non-baselined findings suppressed by `--diff`'s baseline dag.
    pub pre_existing: usize,
    /// Findings frozen by the baseline file.
    pub baselined: usize,
    /// pass / warn / fail from `counted` + `fail_on`.
    pub status: GateStatus,
}

/// The one lint evaluation `check --lint` and `dbtl lint` share: every
/// finding `run_rules` produced, classified into counted / baselined /
/// pre-existing, with the `fail_on` verdict precomputed — so both commands
/// see the same "counted findings" by construction.
pub struct LintGateOutcome {
    /// Every finding `run_rules` produced (exclude/meta suppression already
    /// applied), `baselined` marked — in the deterministic run order.
    pub findings: Vec<Finding>,
    /// Enabled rules that didn't run for a missing input (SQL resolver /
    /// catalog), in `LintRule::ALL` order — `check` prints `SKIP` rows and
    /// `lint` a `skipped:` section; they never count toward the gate.
    pub skipped: Vec<SkippedRule>,
    /// SQL models `run_rules` couldn't parse (the `unparsed` tally both
    /// reports print — a model that never reached a SQL rule, not a
    /// finding).
    pub unparsed: BTreeSet<String>,
    /// Per enabled AND RUNNABLE rule, in `LintRule::ALL` order.
    pub rules: Vec<RuleOutcome>,
    /// Counted findings that meet `fail_on` (the gate's numerator — `0`
    /// when nothing fails).
    pub gating: usize,
    /// `--diff` was in play (drives the "new violations" wording).
    pub has_diff: bool,
    /// The resolved fail threshold.
    pub fail_on: FailOn,
    /// A counted finding at or above `fail_on` exists.
    pub failed: bool,
}

/// The shared lint evaluation: [`dbtl_core::run_rules`] over `dag` with the
/// `aux` inputs, `baseline` applied to the findings, and — with `base` —
/// the same run over the baseline dag narrowing gating to NEW findings (a
/// finding whose [`dbtl_core::Finding::key`] exists on the baseline side is
/// pre-existing noise, the same "new only" idea as the layer-violation
/// gate). With `selection`, only findings whose `node_uid` the selected
/// set contains survive — narrowed BEFORE the baselined/pre-existing
/// bookkeeping, so the report only ever sees selected findings.
pub fn evaluate_lint(
    dag: &Dag,
    cfg: &LintConfig,
    base: Option<&Dag>,
    baseline: Option<&Baseline>,
    fail_on: FailOn,
    aux: LintAux<'_>,
    selection: Option<&BTreeSet<String>>,
) -> LintGateOutcome {
    let outcome = dbtl_core::run_rules(
        dbtl_core::LintInput::new(dag, cfg)
            .with_sql(aux.sql)
            .with_catalog(aux.catalog),
    );
    let mut findings = outcome.findings;
    let skipped = outcome.skipped;
    let mut unparsed = outcome.unparsed;
    if let Some(sel) = selection {
        findings.retain(|f| sel.contains(&f.node_uid));
        // The tally counts models the report's selection could have shown.
        unparsed.retain(|u| sel.contains(u));
    }
    if let Some(b) = baseline {
        b.apply(&mut findings);
    }
    let skipped_rules: BTreeSet<LintRule> = skipped.iter().map(|s| s.rule).collect();
    let base_keys: BTreeSet<(LintRule, String, Vec<String>)> = match base {
        Some(base) => dbtl_core::run_rules(LintInput::new(base, cfg).with_sql(aux.base_sql))
            .findings
            .iter()
            .map(|f| {
                let (r, n, rel) = f.key();
                (r, n.to_string(), rel.to_vec())
            })
            .collect(),
        None => BTreeSet::new(),
    };
    let pre_existing = |f: &Finding| {
        !base_keys.is_empty() && {
            let (r, n, rel) = f.key();
            base_keys.contains(&(r, n.to_string(), rel.to_vec()))
        }
    };
    let mut gating = 0;
    let mut rules = Vec::new();
    for rule in LintRule::ALL {
        let Some(severity) = cfg.severity(rule) else {
            continue;
        };
        if skipped_rules.contains(&rule) {
            continue; // a SKIP row, not a rule verdict
        }
        let mut counted = Vec::new();
        let mut rule_pre_existing = 0;
        let mut rule_baselined = 0;
        for (i, f) in findings.iter().enumerate() {
            if f.rule != rule {
                continue;
            }
            if f.baselined {
                rule_baselined += 1;
            } else if pre_existing(f) {
                rule_pre_existing += 1;
            } else {
                counted.push(i);
                if severity_fails(f.severity, fail_on) {
                    gating += 1;
                }
            }
        }
        let status = if counted.is_empty() {
            GateStatus::Pass
        } else {
            gate_status(severity, fail_on)
        };
        rules.push(RuleOutcome {
            rule,
            severity,
            counted,
            pre_existing: rule_pre_existing,
            baselined: rule_baselined,
            status,
        });
    }
    LintGateOutcome {
        gating,
        has_diff: base.is_some(),
        fail_on,
        failed: gating > 0,
        findings,
        skipped,
        unparsed,
        rules,
    }
}

/// The per-rule gates a [`LintGateOutcome`] produces — shared verbatim between
/// `dbtl lint` (where they ARE the report) and `dbtl check --lint` (where
/// they follow the structural gates), so the same run can never emit two
/// different wordings. Rules evaluated and rules skipped for a missing
/// input interleave in `LintRule::ALL` order — a skipped rule is a `SKIP`
/// gate (`SKIP lint <rule>: <reason>` in text, `status: "skip"` in JSON),
/// informational only, never counted as a failure.
pub fn lint_gates(dag: &Dag, outcome: &LintGateOutcome) -> Vec<GateResult> {
    // The shared display naming the findings themselves use — a source is
    // `<source>.<table>`, so two same-named tables from different sources
    // (`raw_a.orders`/`raw_b.orders`) never collapse into one entry.
    let name = |f: &Finding| dbtl_core::display_name(dag, &f.node_uid);
    let mut gates = Vec::with_capacity(outcome.rules.len() + outcome.skipped.len());
    let mut rules = outcome.rules.iter().peekable();
    let mut skipped = outcome.skipped.iter().peekable();
    // Merge-join the two `LintRule::ALL`-ordered lists so every emitted
    // gate keeps the registry's fixed order.
    loop {
        let take_skip = match (rules.peek(), skipped.peek()) {
            (Some(r), Some(s)) => s.rule < r.rule,
            (None, Some(_)) => true,
            _ => false,
        };
        if take_skip {
            let s = skipped.next().unwrap();
            let id = format!("lint:{}", s.rule.name());
            let title = format!("lint {}", s.rule.name());
            gates.push(GateResult::skip(&id, &title, s.reason.to_string()));
            continue;
        }
        let Some(ro) = rules.next() else {
            break;
        };
        let id = format!("lint:{}", ro.rule.name());
        let title = format!("lint {}", ro.rule.name());
        // Every finding the report keeps for this rule: the counted ones plus
        // the baselined ones (marked) — pre-existing findings stay out of the
        // report entirely (their count is in the pass message).
        let findings: Vec<Finding> = outcome
            .findings
            .iter()
            .enumerate()
            .filter(|(i, f)| f.rule == ro.rule && (ro.counted.contains(i) || f.baselined))
            .map(|(_, f)| f.clone())
            .collect();
        match ro.status {
            GateStatus::Pass => {
                let pass_msg = if outcome.has_diff && (ro.pre_existing > 0 || ro.baselined > 0) {
                    let mut parts = Vec::new();
                    if ro.pre_existing > 0 {
                        parts.push(format!("{} pre-existing", ro.pre_existing));
                    }
                    if ro.baselined > 0 {
                        parts.push(format!("{} baselined", ro.baselined));
                    }
                    format!("no new violations ({})", parts.join(", "))
                } else if ro.baselined > 0 {
                    format!("none ({} baselined)", ro.baselined)
                } else {
                    "none".to_string()
                };
                let mut gate =
                    GateResult::new(&id, &title, ro.severity, GateStatus::Pass, pass_msg);
                gate.findings = findings;
                gates.push(gate);
            }
            // `evaluate_lint` never yields Skip for a rule (skips are for
            // whole checks like --no-breaking without --diff, which live in
            // `run_check` itself) — the arm stays for match-exhaustiveness
            // so a future classification can't silently pass unhandled.
            GateStatus::Skip => {
                gates.push(GateResult::skip(&id, &title, "inputs unavailable".into()));
            }
            GateStatus::Warn | GateStatus::Fail => {
                let names = ro
                    .counted
                    .iter()
                    .map(|&i| name(&outcome.findings[i]))
                    .collect::<Vec<_>>()
                    .join(", ");
                let n = ro.counted.len();
                let msg = format!(
                    "{title}: {n} {}violation(s): {names}",
                    if outcome.has_diff { "new " } else { "" }
                );
                let mut gate = GateResult::new(&id, &title, ro.severity, ro.status, msg);
                gate.findings = findings;
                gates.push(gate);
            }
        }
    }
    gates
}

/// The lint run as report material — the `lint:*` gate rows plus the
/// human-format tallies, ready for a caller to wrap into a [`Report`]
/// (`dbtl lint`) or extend a gate list (`dbtl check --lint`).
pub struct LintAssembly {
    /// One gate per evaluated rule plus one SKIP row per input-skipped
    /// rule, in `LintRule::ALL` order.
    pub gates: Vec<GateResult>,
    /// The summary tallies [`Report::lint`] carries.
    pub tally: LintTally,
    /// Counted findings that meet `fail_on` — the `dbtl lint` verdict
    /// numerator (`> 0` fails the run).
    pub gating: usize,
    /// The fail threshold the verdict was computed against.
    pub fail_on: FailOn,
    /// A counted finding at or above `fail_on` exists.
    pub failed: bool,
}

/// The inputs [`lint_assembly`]/[`lint_report`] evaluate — the run's dag
/// and lint config plus the optional `--diff`, baseline-file, aux-input,
/// `--select`, and text-resolver supplies.
pub struct LintRunInputs<'a> {
    /// The run's dag — findings, `.yml` positions, and gate names all
    /// resolve through it.
    pub dag: &'a Dag,
    /// The resolved lint config.
    pub cfg: &'a LintConfig,
    /// The `--diff` baseline-side dag — enables the new-violation narrowing.
    pub base: Option<&'a Dag>,
    /// The loaded baseline file — its findings are frozen out of gating.
    pub baseline: Option<&'a Baseline>,
    /// The resolved fail threshold.
    pub fail_on: FailOn,
    /// The SQL-resolver / catalog supplies.
    pub aux: LintAux<'a>,
    /// The `--select` node set (`None` = the whole run).
    pub selection: Option<&'a BTreeSet<String>>,
    /// Resolve a project-relative `.yml` path into the file's text — the
    /// one source finding positions and report excerpts both read from
    /// (`None` leaves positions unresolved, exactly like an unreadable
    /// file).
    pub read_text: &'a dyn Fn(&str) -> Option<String>,
}

/// Evaluate lint for a run and package it: `evaluate_lint`, fill the
/// findings' `.yml` positions through `read_text` (the same source
/// [`crate::collect_sources`] excerpts from, so a position and its excerpt
/// always come from the same text), and lift into gate rows + tallies.
pub fn lint_assembly(inputs: &LintRunInputs<'_>) -> LintAssembly {
    let mut outcome = evaluate_lint(
        inputs.dag,
        inputs.cfg,
        inputs.base,
        inputs.baseline,
        inputs.fail_on,
        inputs.aux,
        inputs.selection,
    );
    dbtl_core::fill_finding_lines_with(&mut outcome.findings, inputs.dag, inputs.read_text);
    let gates = lint_gates(inputs.dag, &outcome);
    let tally = LintTally {
        pre_existing: outcome.rules.iter().map(|r| r.pre_existing).sum(),
        has_diff: outcome.has_diff,
        skipped: outcome.skipped,
        unparsed_models: outcome
            .unparsed
            .iter()
            .map(|u| dbtl_core::display_name(inputs.dag, u))
            .collect(),
    };
    LintAssembly {
        gates,
        tally,
        gating: outcome.gating,
        fail_on: outcome.fail_on,
        failed: outcome.failed,
    }
}

/// `dbtl lint`'s product: the finished [`Report`] plus the gate verdict —
/// the report carries the findings, the caller decides the exit code.
pub struct LintRun {
    /// The run's report — gates, findings, tallies all populated.
    pub report: Report,
    /// Counted findings that meet `fail_on` (the `dbtl lint` numerator).
    pub gating: usize,
    /// The fail threshold in effect — echoed in the caller's error.
    pub fail_on: FailOn,
    /// A counted finding at or above `fail_on` exists.
    pub failed: bool,
}

/// The whole `dbtl lint` evaluation as a [`Report`]: [`lint_assembly`]'s
/// gates and tallies under the caller's `project`/`scope` label.
pub fn lint_report(project: &str, scope: &str, inputs: &LintRunInputs<'_>) -> LintRun {
    let a = lint_assembly(inputs);
    let mut report = Report::new(project, scope);
    report.gates = a.gates;
    report.summary = Report::summarize(&report.gates);
    report.lint = Some(a.tally);
    LintRun {
        report,
        gating: a.gating,
        fail_on: a.fail_on,
        failed: a.failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A model node literal — `path` carries the layer dir like a real
    /// manifest (`staging/x.sql`).
    fn model(name: &str, path: &str) -> (String, dbtl_core::RawNode) {
        (
            format!("model.p.{name}"),
            dbtl_core::RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(path.into()),
                ..Default::default()
            },
        )
    }

    /// A source node literal — `source.p.<src>.<tbl>` like a real manifest
    /// (`NodeInfo.name` carries the bare table name; `display_name`
    /// qualifies it with the source segment).
    fn source(src: &str, tbl: &str) -> (String, dbtl_core::RawSource) {
        (
            format!("source.p.{src}.{tbl}"),
            dbtl_core::RawSource {
                name: tbl.into(),
                resource_type: "source".into(),
                ..Default::default()
            },
        )
    }

    /// A manifest assembled from the given models + (parent, child) edges;
    /// `child_map` derives from `edges` so each test writes its graph once.
    fn manifest(
        nodes: Vec<(String, dbtl_core::RawNode)>,
        edges: &[(&str, &str)],
    ) -> dbtl_core::RawManifest {
        manifest_with(nodes, Vec::new(), edges)
    }

    /// [`manifest`] plus a `sources` map — the gate-naming tests need real
    /// source nodes, not just models.
    fn manifest_with(
        nodes: Vec<(String, dbtl_core::RawNode)>,
        sources: Vec<(String, dbtl_core::RawSource)>,
        edges: &[(&str, &str)],
    ) -> dbtl_core::RawManifest {
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
        dbtl_core::RawManifest {
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
            docs: Default::default(),
        }
    }

    /// A config with ONLY `rules` enabled — everything else off — so a test
    /// sees exactly the findings it means to.
    fn cfg_only(rules: &[LintRule]) -> LintConfig {
        let mut cfg = LintConfig::default();
        for r in LintRule::ALL {
            if !rules.contains(&r) {
                cfg.severities.insert(r, None);
            }
        }
        cfg
    }

    /// Two parentless models — two `root_models` findings, nothing else
    /// when scoped to that rule.
    fn two_root_models() -> Dag {
        Dag::build(&manifest(
            vec![model("a", "marts/a.sql"), model("b", "marts/b.sql")],
            &[],
        ))
    }

    /// No auxiliary inputs — graph-only evaluation (the SQL/catalog rules
    /// would all skip if enabled; the tests scope to graph rules anyway).
    fn no_aux() -> LintAux<'static> {
        LintAux {
            sql: None,
            catalog: None,
            base_sql: None,
        }
    }

    #[test]
    fn the_gate_status_table() {
        // (finding severity, fail_on) → fails?
        for (sev, fail_on, want) in [
            (Severity::Error, FailOn::Error, true),
            (Severity::Error, FailOn::Warn, true),
            (Severity::Error, FailOn::Never, false),
            (Severity::Warn, FailOn::Error, false),
            (Severity::Warn, FailOn::Warn, true),
            (Severity::Warn, FailOn::Never, false),
        ] {
            assert_eq!(
                severity_fails(sev, fail_on),
                want,
                "{sev:?} under {fail_on:?}"
            );
            let want_status = if want {
                GateStatus::Fail
            } else {
                GateStatus::Warn
            };
            assert_eq!(
                gate_status(sev, fail_on),
                want_status,
                "{sev:?} under {fail_on:?}"
            );
        }
    }

    #[test]
    fn evaluate_partitions_findings_into_counted_baselined_pre_existing() {
        let dag = two_root_models();
        let cfg = cfg_only(&[LintRule::MissingPrimaryKeyTests]);
        // Plain run: both findings count, gate fails under the default.
        let out = evaluate_lint(&dag, &cfg, None, None, FailOn::Error, no_aux(), None);
        assert_eq!(out.rules[0].counted.len(), 2);
        assert_eq!(out.rules[0].baselined, 0);
        assert!(out.failed && out.rules[0].status == GateStatus::Fail);
        // A baseline freezing `a` leaves only `b` counted.
        let frozen = Baseline::from_findings(
            &out.findings
                .iter()
                .filter(|f| f.node_uid == "model.p.a")
                .cloned()
                .collect::<Vec<_>>(),
        );
        let out = evaluate_lint(
            &dag,
            &cfg,
            None,
            Some(&frozen),
            FailOn::Error,
            no_aux(),
            None,
        );
        assert_eq!(out.rules[0].counted.len(), 1);
        assert_eq!(out.rules[0].baselined, 1);
        assert_eq!(out.findings[out.rules[0].counted[0]].node_uid, "model.p.b");
        // Self-diff: every finding is pre-existing — nothing counts.
        let out = evaluate_lint(&dag, &cfg, Some(&dag), None, FailOn::Error, no_aux(), None);
        assert_eq!(out.rules[0].counted.len(), 0);
        assert_eq!(out.rules[0].pre_existing, 2);
        assert!(!out.failed && out.rules[0].status == GateStatus::Pass);
        // Diff + baseline: the frozen one is baselined, the rest pre-existing.
        let out = evaluate_lint(
            &dag,
            &cfg,
            Some(&dag),
            Some(&frozen),
            FailOn::Error,
            no_aux(),
            None,
        );
        assert_eq!(out.rules[0].counted.len(), 0);
        assert_eq!(out.rules[0].pre_existing, 1);
        assert_eq!(out.rules[0].baselined, 1);
    }

    #[test]
    fn evaluate_honours_fail_on_for_warn_findings() {
        let dag = two_root_models();
        let mut cfg = cfg_only(&[LintRule::RootModels]);
        cfg.severities
            .insert(LintRule::RootModels, Some(Severity::Warn));
        // Warn findings under the default `error` threshold report but pass.
        let out = evaluate_lint(&dag, &cfg, None, None, FailOn::Error, no_aux(), None);
        assert_eq!(out.rules[0].counted.len(), 2);
        assert!(!out.failed);
        assert_eq!(out.rules[0].status, GateStatus::Warn);
        // `--fail-on warn` fails on them.
        let out = evaluate_lint(&dag, &cfg, None, None, FailOn::Warn, no_aux(), None);
        assert!(out.failed && out.gating == 2);
        // `never` never fails.
        let out = evaluate_lint(&dag, &cfg, None, None, FailOn::Never, no_aux(), None);
        assert!(!out.failed && out.rules[0].status == GateStatus::Warn);
    }

    /// Two same-named tables in different sources: the gate's node list
    /// qualifies each with its source (`raw_a.orders, raw_b.orders`), where
    /// the old bare-`name` list collapsed both to `orders, orders`. The
    /// findings themselves already render the qualified `display_name`.
    #[test]
    fn gate_message_qualifies_same_named_source_tables() {
        let dag = Dag::build(&manifest_with(
            vec![model("x", "marts/x.sql")],
            vec![source("raw_a", "orders"), source("raw_b", "orders")],
            &[],
        ));
        let cfg = cfg_only(&[LintRule::UnusedSources]);
        let outcome = evaluate_lint(&dag, &cfg, None, None, FailOn::Error, no_aux(), None);
        let gates = lint_gates(&dag, &outcome);
        let gate = gates
            .iter()
            .find(|g| g.id == "lint:unused_sources")
            .expect("the unused_sources gate");
        assert_eq!(
            gate.message,
            "lint unused_sources: 2 violation(s): raw_a.orders, raw_b.orders"
        );
    }
}
