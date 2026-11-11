use super::*;

// ---- --diff baseline + Diff lens ------------------------------------------

#[test]
fn diff_open_without_baseline_toasts_a_hint() {
    let mut a = app();
    assert!(a.diff().is_none(), "no baseline by default");
    assert_eq!(a.diff_status_label(), None, "no chip without a baseline");
    let out = apply_action(&mut a, Action::DiffOpen);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.mode, Mode::Selection, "no modal without a baseline");
    let note = a
        .take_notice()
        .expect("DiffOpen without a baseline notices");
    assert!(note.contains("--diff"), "the hint names the flag: {note}");
}

#[test]
fn cycle_lens_skips_diff_without_a_baseline_and_includes_it_with_one() {
    // Without a baseline the apply_action cycle skips the Diff slot (the
    // fixture's lint findings keep the Lint slot live).
    let mut a = app();
    for expected in [
        LineageLens::Coverage,
        LineageLens::DegreeHeat,
        LineageLens::Layer,
        LineageLens::LayerViolation,
        LineageLens::Lint,
        LineageLens::PrimaryKey,
        LineageLens::Off,
    ] {
        apply_action(&mut a, Action::CycleLens);
        assert_eq!(a.ui_state.lens(), expected, "Diff is skipped");
    }
    // With a baseline the Diff slot joins the ring after Lint.
    let mut a = diff_app();
    a.ui_state.set_lens(LineageLens::Off);
    for expected in [
        LineageLens::Coverage,
        LineageLens::DegreeHeat,
        LineageLens::Layer,
        LineageLens::LayerViolation,
        LineageLens::Lint,
        LineageLens::Diff,
        LineageLens::PrimaryKey,
        LineageLens::Off,
    ] {
        apply_action(&mut a, Action::CycleLens);
        assert_eq!(a.ui_state.lens(), expected, "Diff joins the cycle");
    }
}

#[test]
fn diff_status_label_counts_and_reports_clean() {
    let a = diff_app();
    assert_eq!(a.diff_status_label().as_deref(), Some("diff +1 ~1 -1"));
    // A baseline identical to the current Dag reads as clean.
    let mut a = app();
    let same = load_dag(FIXTURE).expect("fixture loads");
    a.set_diff_base(same, FIXTURE.to_string());
    assert_eq!(a.diff_status_label().as_deref(), Some("diff clean"));
}

#[test]
fn diff_open_snapshots_names_reasons_and_edges_into_the_modal_payload() {
    let mut a = diff_app();
    let out = apply_action(&mut a, Action::DiffOpen);
    assert!(!out.quit && out.effects.is_empty());
    let Mode::Diff(dv) = &a.mode else {
        panic!("DiffOpen with a baseline opens the modal, got {:?}", a.mode);
    };
    assert_eq!(dv.baseline, "base/manifest.json");
    assert_eq!(dv.added, vec![("c".to_string(), "model".to_string())]);
    assert_eq!(dv.removed, vec![("gone".to_string(), "model".to_string())]);
    assert_eq!(
        dv.modified,
        vec![(
            "b".to_string(),
            "materialized: table -> incremental".to_string()
        )]
    );
    assert_eq!(dv.edges_added, vec![("b".to_string(), "c".to_string())]);
    assert!(dv.edges_removed.is_empty());
    // The shared modal-scroll slot drives this modal too (the
    // modal_scroll_mut enumeration includes Diff).
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    let Mode::Diff(dv) = &a.mode else {
        unreachable!()
    };
    assert_eq!(dv.scroll, 1, "j scrolls the diff modal");
    apply_action(&mut a, Action::DetailClose);
    assert_eq!(a.mode, Mode::Selection, "q/Esc closes it");
}

#[test]
fn diff_lens_tints_added_and_modified_nodes_only() {
    let mut a = diff_app();
    a.select_by_unique_id("model.p.b");
    a.ui_state.set_lens(LineageLens::Diff);
    let lay = crate::layout(&a.lineage_subgraph());
    let styles = a.lineage_styles(&lay);
    assert_eq!(
        styles.get("model.p.c").unwrap().lens,
        LensTint::DiffAdd,
        "an added node tints green"
    );
    assert_eq!(
        styles.get("model.p.b").unwrap().lens,
        LensTint::DiffMod,
        "a modified node tints amber"
    );
    assert_eq!(
        styles.get("model.p.a").unwrap().lens,
        LensTint::None,
        "an unchanged node keeps its class colour"
    );
}

#[test]
fn set_diff_base_invalidates_the_cached_styled_layout() {
    // The Diff lens reads App state OUTSIDE the layout key's other inputs, so
    // loading a baseline must bump the generation — otherwise a layout cached
    // under lens=Diff/no-baseline would survive with no tints.
    let mut a = App::new(
        diff_current_dag(),
        PathBuf::from("/tmp/x/target/manifest.json"),
    );
    a.select_by_unique_id("model.p.b");
    a.ui_state.set_lens(LineageLens::Diff);
    let before = a.styled_lineage_layout().expect("layout");
    a.set_diff_base(diff_base_dag(), "base".to_string());
    let after = a.styled_lineage_layout().expect("layout");
    assert!(
        !std::rc::Rc::ptr_eq(&before, &after),
        "the cached styled layout misses after set_diff_base"
    );
}

#[test]
fn set_sql_dialect_invalidates_the_cached_styled_layout() {
    // `--sql-dialect` rebuilds BOTH resolvers and re-runs the diff: resolved
    // columns feed the column trace and the diff feeds the Diff lens, so a
    // layout cached under the old dialect must miss (generation bump).
    let mut a = App::new(
        diff_current_dag(),
        PathBuf::from("/tmp/x/target/manifest.json"),
    );
    a.set_diff_base(diff_base_dag(), "base".to_string());
    a.select_by_unique_id("model.p.b");
    let before = a.styled_lineage_layout().expect("layout");
    a.set_sql_dialect(crate::SqlDialect::Snowflake);
    let after = a.styled_lineage_layout().expect("layout");
    assert!(
        !std::rc::Rc::ptr_eq(&before, &after),
        "the cached styled layout misses after set_sql_dialect"
    );
}

#[test]
fn reload_recomputes_the_diff_against_the_kept_baseline() {
    // Fixture app + a synthetic baseline: everything in the fixture reads as
    // added. A reload (same file) must re-diff — not drop or stale the result.
    let mut a = app();
    a.set_diff_base(diff_base_dag(), "base".to_string());
    let before = a.diff().expect("diff computed").counts();
    assert_eq!(before.0, 93, "every fixture node is added vs the tiny base");
    a.reload().expect("reload ok");
    let after = a.diff().expect("diff survives reload").counts();
    assert_eq!(before, after, "reload re-diffs against the same baseline");
}

// ---- PR Impact Pack (issue #36): the reviewer-shaped report over --diff ----

/// Open the `D` modal on `diff_app` and return its snapshotted `PrImpact`.
fn pr_impact_of(a: &mut App) -> crate::PrImpact {
    apply_action(a, Action::DiffOpen);
    let Mode::Diff(dv) = &a.mode else {
        panic!("DiffOpen with a baseline opens the modal");
    };
    dv.pr.clone()
}

#[test]
fn pr_impact_packs_ci_command_and_flags_untested_for_the_diff_fixture() {
    // diff_app: base a->b(+gone), current a->b(modified)->c(added). The only
    // downstream of the changed set {b,c} is c itself — which is changed, so the
    // aggregate blast radius is empty. Both changed models are untested.
    let mut a = diff_app();
    let pr = pr_impact_of(&mut a);
    assert_eq!(
        pr.affected_models, 0,
        "c is changed, so it isn't 'affected'"
    );
    assert!(pr.affected_marts.is_empty());
    assert!(pr.affected_exposures.is_empty());
    assert_eq!(
        pr.ci_command.as_deref(),
        Some("dbt build --select b+ c+"),
        "added + modified buildable nodes, each with a downstream selector"
    );
    assert_eq!(
        pr.untested_changes,
        vec!["b".to_string(), "c".to_string()],
        "both changed models carry no tests"
    );
    assert!(pr.changed_hubs.is_empty(), "neither has wide fan-out");
    assert!(pr.new_layer_violations.is_empty());
}

#[test]
fn pr_impact_aggregates_blast_radius_marts_and_exposures() {
    // base: stg_a -> {fct_x, fct_y}. current: stg_a SQL-modified (untested),
    // the two marts unchanged, plus a dashboard consuming fct_x.
    let base = pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 1")),
            ("model.p.fct_x", pr_model("fct_x", "marts", "select 2")),
            ("model.p.fct_y", pr_model("fct_y", "marts", "select 3")),
        ],
        &[
            ("model.p.stg_a", "model.p.fct_x"),
            ("model.p.stg_a", "model.p.fct_y"),
        ],
        vec![],
    );
    let dash = crate::RawExposure {
        name: "exec_dash".into(),
        exposure_type: Some("dashboard".into()),
        owner: crate::RawExposureOwner {
            name: Some("Finance".into()),
            email: Some("fin@example.com".into()),
        },
        depends_on: crate::RawDependsOn {
            nodes: vec!["model.p.fct_x".into()],
            macros: vec![],
        },
        ..Default::default()
    };
    let current = pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 999")),
            ("model.p.fct_x", pr_model("fct_x", "marts", "select 2")),
            ("model.p.fct_y", pr_model("fct_y", "marts", "select 3")),
        ],
        &[
            ("model.p.stg_a", "model.p.fct_x"),
            ("model.p.stg_a", "model.p.fct_y"),
        ],
        vec![("exposure.p.exec_dash", dash)],
    );
    let mut a = App::new(current, PathBuf::from("/p/target/manifest.json"));
    a.set_diff_base(base, "base".into());
    let pr = pr_impact_of(&mut a);

    assert_eq!(pr.affected_models, 2, "both marts are downstream of stg_a");
    assert_eq!(
        pr.affected_marts,
        vec!["fct_x".to_string(), "fct_y".to_string()]
    );
    assert_eq!(
        pr.affected_exposures,
        vec!["exec_dash (dashboard, owner: Finance <fin@example.com>)".to_string()],
        "the downstream exposure is the 'who cares' line"
    );
    assert_eq!(pr.ci_command.as_deref(), Some("dbt build --select stg_a+"));
    assert_eq!(pr.untested_changes, vec!["stg_a".to_string()]);
    assert!(pr.changed_hubs.is_empty(), "2 downstream < hub threshold");
    assert!(pr.new_layer_violations.is_empty());
}

#[test]
fn pr_impact_flags_changed_hubs_at_the_threshold() {
    use crate::PR_HUB_MIN_DOWNSTREAM as MIN;
    // A hub fanning out to exactly MIN marts; modified by an SQL change.
    let children: Vec<String> = (0..MIN).map(|i| format!("model.p.c{i}")).collect();
    let edges: Vec<(&str, &str)> = children
        .iter()
        .map(|c| ("model.p.hub", c.as_str()))
        .collect();
    let mut nodes = vec![("model.p.hub", pr_model("hub", "staging", "select 1"))];
    for (i, uid) in children.iter().enumerate() {
        nodes.push((
            uid.as_str(),
            pr_model(&format!("c{i}"), "marts", "select 0"),
        ));
    }
    let base = pr_dag(nodes.clone(), &edges, vec![]);
    // current: same graph, hub's SQL changed.
    let mut cur_nodes = nodes.clone();
    cur_nodes[0] = ("model.p.hub", pr_model("hub", "staging", "select 2"));
    let current = pr_dag(cur_nodes, &edges, vec![]);

    let mut a = App::new(current, PathBuf::from("/p/target/manifest.json"));
    a.set_diff_base(base, "base".into());
    let pr = pr_impact_of(&mut a);
    assert_eq!(
        pr.changed_hubs,
        vec![("hub".to_string(), MIN)],
        "a changed node at the downstream threshold is a hub risk"
    );
    assert_eq!(pr.affected_models, MIN, "all MIN children are downstream");
}

#[test]
fn pr_impact_reports_only_newly_introduced_layer_violations() {
    // base: stg_a -> fct_x (clean). current adds `bad` (staging) fed BY fct_x —
    // a marts->staging backward edge that did not exist in the baseline.
    let base = pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 1")),
            ("model.p.fct_x", pr_model("fct_x", "marts", "select 2")),
        ],
        &[("model.p.stg_a", "model.p.fct_x")],
        vec![],
    );
    let current = pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 1")),
            ("model.p.fct_x", pr_model("fct_x", "marts", "select 2")),
            ("model.p.bad", pr_model("bad", "staging", "select 3")),
        ],
        &[
            ("model.p.stg_a", "model.p.fct_x"),
            ("model.p.fct_x", "model.p.bad"),
        ],
        vec![],
    );
    let mut a = App::new(current, PathBuf::from("/p/target/manifest.json"));
    a.set_diff_base(base, "base".into());
    let pr = pr_impact_of(&mut a);
    assert_eq!(
        pr.new_layer_violations,
        vec![("fct_x".to_string(), "bad".to_string())],
        "the new backward edge is flagged, baseline-only ones are not"
    );
}

#[test]
fn pr_impact_markdown_is_pasteable_and_deterministic() {
    let a = diff_app();
    let dv = a.compute_diff_view().expect("baseline loaded");
    let md = a.pr_impact_markdown(&dv);
    assert!(md.starts_with("# PR Impact Report: p"), "{md}");
    assert!(md.contains("- baseline: `base/manifest.json`"), "{md}");
    assert!(
        md.contains("- changed: +1 added, ~1 modified, -1 removed"),
        "{md}"
    );
    assert!(md.contains("## Added (1)"), "{md}");
    assert!(md.contains("- c (model)"), "{md}");
    assert!(md.contains("## Suggested CI command"), "{md}");
    assert!(
        md.contains("```sh\ndbt build --select b+ c+\n```"),
        "the CI command is fenced for copy-paste: {md}"
    );
    assert!(md.contains("### Untested changes (2)"), "{md}");
    // Pure formatter over the snapshot → byte-identical on repeat.
    assert_eq!(md, a.pr_impact_markdown(&dv), "deterministic");
}

#[test]
fn export_pr_impact_writes_markdown_only_inside_the_diff_modal() {
    let mut a = diff_app();
    // Outside the modal it is a no-op (it reads the open DiffView snapshot).
    let out = apply_action(&mut a, Action::ExportPrImpact);
    assert!(out.effects.is_empty(), "no export without the modal open");

    apply_action(&mut a, Action::DiffOpen);
    let out = apply_action(&mut a, Action::ExportPrImpact);
    let [Effect::WriteFile { path, contents }] = out.effects.as_slice() else {
        panic!(
            "ExportPrImpact in the modal writes a file: {:?}",
            out.effects
        );
    };
    assert_eq!(path, "p_pr_impact.md", "named for the project");
    assert!(contents.contains("# PR Impact Report"), "{contents}");
    assert_eq!(a.take_notice().as_deref(), Some("Exported p_pr_impact.md"));
}

#[test]
fn yank_pr_selector_copies_the_ci_command_from_the_modal() {
    let mut a = diff_app();
    apply_action(&mut a, Action::DiffOpen);
    let out = apply_action(&mut a, Action::YankPrSelector);
    assert!(
        matches!(out.effects.as_slice(), [Effect::Yank(t)] if t == "dbt build --select b+ c+"),
        "yanks the changed-set selector: {:?}",
        out.effects
    );
    // The fixture's nodes carry no column data — the narrowed selector keeps
    // the same `+` fan-out, and the notice says which surface was copied.
    assert_eq!(
        a.take_notice().as_deref(),
        Some("Copied column-level dbt build command")
    );
}

#[test]
fn yank_pr_selector_prefers_narrow_and_shift_y_copies_wide() {
    // A modal whose narrowed selector differs from the wide one: `y` takes
    // the column-level command, `Y` the wide fan-out.
    let mut a = diff_app();
    a.mode = Mode::Diff(DiffView {
        pr: crate::PrImpact {
            ci_command: Some("dbt build --select a+ b+".to_string()),
            ci_command_columns: Some("dbt build --select a mart_b".to_string()),
            ..Default::default()
        },
        ..Default::default()
    });
    let out = apply_action(&mut a, Action::YankPrSelector);
    assert!(
        matches!(out.effects.as_slice(), [Effect::Yank(t)] if t == "dbt build --select a mart_b"),
        "`y` prefers the column-narrowed selector: {:?}",
        out.effects
    );
    assert_eq!(
        a.take_notice().as_deref(),
        Some("Copied column-level dbt build command")
    );
    let out = apply_action(&mut a, Action::YankPrSelectorWide);
    assert!(
        matches!(out.effects.as_slice(), [Effect::Yank(t)] if t == "dbt build --select a+ b+"),
        "`Y` always takes the wide command: {:?}",
        out.effects
    );
    assert_eq!(a.take_notice().as_deref(), Some("Copied dbt build command"));
}

#[test]
fn yank_pr_selector_notices_when_nothing_buildable_changed() {
    // A baseline identical to the current dag: no changes, so no command.
    let mut a = App::new(diff_current_dag(), PathBuf::from("/p/target/manifest.json"));
    a.set_diff_base(diff_current_dag(), "base".into());
    apply_action(&mut a, Action::DiffOpen);
    let out = apply_action(&mut a, Action::YankPrSelector);
    assert!(out.effects.is_empty(), "nothing to copy");
    assert_eq!(
        a.take_notice().as_deref(),
        Some("No changed models to build")
    );
}
