use super::*;
use crate::action::{palette_candidates, Action, Direction, Mode, SearchTarget};
use crate::effect::Effect;
use crate::{
    coverage_gap, load_dag_from_source, reduce_selection, Dag, Focus, LensTint, LineageLens,
    MaterializationClass, NodeInfo, SortMode, UiState,
};

use std::path::{Path, PathBuf};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

fn app() -> App {
    let dag = load_dag(FIXTURE).expect("fixture loads");
    App::new(dag, PathBuf::from(FIXTURE))
}

#[test]
fn yank_and_bookmark_record_oneshot_notices() {
    let mut a = app();
    assert_eq!(a.take_notice(), None, "fresh app has no pending notice");
    let out = apply_action(&mut a, Action::YankId);
    assert!(
        matches!(out.effects.as_slice(), [Effect::Yank(_)]),
        "yank still requests the clipboard effect"
    );
    assert_eq!(a.take_notice().as_deref(), Some("Copied unique_id"));
    assert_eq!(a.take_notice(), None, "take_notice drains (one-shot)");

    apply_action(&mut a, Action::BookmarkToggle);
    let note = a.take_notice().expect("bookmark toggle notices");
    assert!(note.starts_with("Bookmarked: "), "got {note:?}");
    apply_action(&mut a, Action::BookmarkToggle);
    let note = a.take_notice().expect("un-bookmark notices");
    assert!(note.starts_with("Removed bookmark: "), "got {note:?}");
}

#[test]
fn theme_cycle_wraps_the_loaded_list_and_notices_the_landing_name() {
    let mut a = app();
    assert_eq!(a.theme_name(), "default", "fresh app starts on the default");
    assert_eq!(a.active_theme(), &crate::ui::theme::DEFAULT);

    // A full lap over the built-in presets lands back on the default, and
    // every step records a `theme: {name}` toast.
    let count = crate::ui::theme::preset_names().len();
    for _ in 0..count {
        let out = apply_action(&mut a, Action::ThemeCycle);
        assert!(out.effects.is_empty(), "a theme cycle is pure state");
        let note = a.take_notice().expect("cycle notices the landing theme");
        assert_eq!(note, format!("theme: {}", a.theme_name()));
    }
    assert_eq!(a.theme_name(), "default", "a full lap wraps around");

    // set_themes (the --theme seam): the list is replaced, the index selects,
    // an out-of-range index clamps, and an empty list is ignored.
    let ayu = crate::ui::theme::preset("ayu-dark").expect("preset");
    a.set_themes(
        vec![
            ("default".into(), crate::ui::theme::DEFAULT),
            ("mine".into(), ayu),
        ],
        1,
    );
    assert_eq!(a.theme_name(), "mine");
    assert_eq!(a.active_theme(), &ayu);
    a.set_themes(Vec::new(), 99);
    assert_eq!(a.theme_name(), "mine", "empty list ignored, index clamped");
}

#[test]
fn export_notice_names_the_written_path() {
    let mut a = app();
    let out = apply_action(&mut a, Action::ExportLineage);
    let Some(Effect::WriteFile { path, .. }) = out.effects.first() else {
        panic!("export must request the write effect");
    };
    assert_eq!(a.take_notice(), Some(format!("Exported {path}")));
}

#[test]
fn apply_action_forwards_uistate_arms_like_handle_key() {
    // For the legacy keys UNDER LIST FOCUS, apply_action must mutate
    // ui_state identically to driving reduce_selection directly (the
    // wrapping relationship is exact). Under lineage focus the movement
    // keys intentionally diverge: they drive the App-level lineage cursor,
    // which the bare UiState cannot represent (see the cursor tests below).
    let mut a = app();
    let mut bare = UiState::new(a.model_list.len());
    for action in [
        Action::MoveDown,
        Action::MoveDown,
        Action::JumpBottom,
        Action::MoveUp,
    ] {
        let out = apply_action(&mut a, action);
        assert!(!out.quit && out.effects.is_empty());
        reduce_selection(&mut bare, action);
        assert_eq!(
            a.ui_state.selected(),
            bare.selected(),
            "{action:?} divergent"
        );
    }
}

#[test]
fn quit_action_sets_quit() {
    let mut a = app();
    assert!(apply_action(&mut a, Action::Quit).quit);
}

#[test]
fn unimplemented_domain_actions_are_safe_noops() {
    let mut a = app();
    let before = a.ui_state.selected();
    for action in [
        Action::HelpToggle,
        Action::DetailOpen,
        Action::SearchOpen,
        Action::DetailScroll(Direction::Down),
    ] {
        let out = apply_action(&mut a, action);
        assert!(!out.quit && out.effects.is_empty());
    }
    assert_eq!(
        a.ui_state.selected(),
        before,
        "no-op domain actions don't move selection"
    );
}

// ---- blast radius (impact_counts) ----

#[test]
fn impact_counts_match_frozen_fixture_closures() {
    // Anchored to the SAME closures the fixture closure tests freeze:
    // fct_subscription_process → 2 downstream / 27 upstream (manifest_fixture
    // `closure_deep_multihop_fct_subscription_process`).
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    assert_eq!(a.impact_counts(), Some((2, 27)), "fct blast radius");
    // stg_payment__shoppers → 1 downstream / 2 upstream (closure_both…).
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    assert_eq!(a.impact_counts(), Some((1, 2)), "stg shoppers blast radius");
}

#[test]
fn impact_counts_for_targets_the_given_node_not_the_root() {
    // The detail modal targets the FOCUS uid (a non-root source/seed/snapshot
    // under the lineage cursor), so the helper must count THAT node.
    let a = app();
    // A source with two downstream models (closure_downstream_only_shoppers…).
    let src = "source.jaffle_finance.dev_lake_jaffle_payment.shoppers";
    assert_eq!(a.impact_counts_for(src), (2, 0), "source down/up");
}

#[test]
fn impact_status_uses_chrome_badges_per_glyph_mode() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    a.glyph_mode = crate::GlyphMode::Unicode;
    assert_eq!(a.impact_status().as_deref(), Some("impact ↓1 ↑2"));
    a.glyph_mode = crate::GlyphMode::Ascii;
    assert_eq!(
        a.impact_status().as_deref(),
        Some("impact v1 ^2"),
        "ascii mode uses v/^ badges, never the unicode arrows"
    );
}

#[test]
fn impact_splits_exposures_out_of_the_counts_and_chips_them() {
    // fct's Dag closure holds 4 descendants (2 models + the 2 fixture
    // exposures); the chip/report counts keep meaning buildable resources, so
    // the split yields (2, 27, 2) and the chip gains ` exp:2` in BOTH glyph
    // modes (the suffix is ASCII by construction).
    let fct = "model.jaffle_finance.fct_subscription_process";
    let mut a = app();
    a.select_by_unique_id(fct);
    assert_eq!(
        a.dag.downstream(fct).len(),
        4,
        "raw closure incl. exposures"
    );
    assert_eq!(a.impact_counts(), Some((2, 27)), "counts exclude exposures");
    assert_eq!(a.impact_breakdown_for(fct), (2, 27, 2));
    a.glyph_mode = crate::GlyphMode::Unicode;
    assert_eq!(a.impact_status().as_deref(), Some("impact ↓2 ↑27 exp:2"));
    a.glyph_mode = crate::GlyphMode::Ascii;
    assert_eq!(a.impact_status().as_deref(), Some("impact v2 ^27 exp:2"));
    // The exposure list is name-sorted and carries the side-map payloads.
    let exps = a.downstream_exposures(fct);
    let names: Vec<&str> = exps.iter().map(|(n, _)| n.name.as_str()).collect();
    assert_eq!(
        names,
        ["delivery_kpi_dashboard", "subscription_churn_notebook"]
    );
    assert_eq!(
        exps[0].1.unwrap().exposure_type.as_deref(),
        Some("dashboard")
    );
    // A node with no downstream exposures keeps the pre-exposure chip
    // byte-identical (asserted exactly in impact_status_uses_chrome_badges).
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    assert_eq!(
        a.impact_breakdown_for("model.jaffle_finance.stg_payment__shoppers")
            .2,
        0
    );
}

// ---- lineage breadcrumb ----

#[test]
fn breadcrumb_is_none_with_empty_history() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    assert!(a.back.is_empty());
    assert_eq!(a.breadcrumb(200), None, "no history → no breadcrumb");
}

#[test]
fn breadcrumb_shows_last_three_history_names_then_root() {
    // Drive the public API: each jump_to pushes the prior root onto `back`.
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    // fct → rpt_delivery_base_metrics → fct_delivery_monthly_snapshot
    a.jump_to("model.jaffle_finance.rpt_delivery_base_metrics");
    a.jump_to("model.jaffle_finance.fct_delivery_monthly_snapshot");
    assert_eq!(
        a.breadcrumb(200).as_deref(),
        Some(
            "fct_subscription_process > rpt_delivery_base_metrics > fct_delivery_monthly_snapshot"
        ),
        "breadcrumb = back history names then the current root"
    );
}

#[test]
fn breadcrumb_caps_at_three_history_entries_plus_root() {
    // A four-deep back stack: only the LAST 3 history entries show + the root,
    // so the oldest (a > ) drops even before truncation.
    let mut a = app();
    a.back = vec![
        "model.jaffle_finance.pos_txn".into(),
        "model.jaffle_finance.pos_pay".into(),
        "model.jaffle_finance.stg_payment__shoppers".into(),
        "model.jaffle_finance.int_shoppers__combined".into(),
    ];
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    // 3 newest history (pos_pay, stg…, int…) + root; pos_txn is dropped.
    assert_eq!(
        a.breadcrumb(200).as_deref(),
        Some("pos_pay > stg_payment__shoppers > int_shoppers__combined > fct_subscription_process"),
    );
}

#[test]
fn fit_breadcrumb_is_strict_and_drops_to_none() {
    // The pure helper shared by the producer and the draw seam: it fits by
    // dropping whole LEFT entries then a ".." prefix, and returns None (strict)
    // when even ".. > root" cannot fit — the draw seam needs that to drop the
    // crumb to empty rather than overrun the title suffixes.
    let full = "alpha > beta > gamma";
    assert_eq!(
        fit_breadcrumb(full, 100).as_deref(),
        Some(full),
        "fits whole"
    );
    // Width holds ".. > beta > gamma" (17) but not the full 20.
    assert_eq!(
        fit_breadcrumb(full, 17).as_deref(),
        Some(".. > beta > gamma"),
        "drop oldest, prefix .."
    );
    // Width holds only ".. > gamma" (10).
    assert_eq!(fit_breadcrumb(full, 10).as_deref(), Some(".. > gamma"));
    // Too narrow even for ".. > gamma": strict None (never an ellipsis char).
    assert_eq!(
        fit_breadcrumb(full, 5),
        None,
        "strict: None when nothing fits"
    );
}

#[test]
fn breadcrumb_truncates_whole_entries_from_the_left_with_dotdot() {
    let mut a = app();
    a.back = vec!["model.jaffle_finance.stg_payment__shoppers".into()];
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let full = "stg_payment__shoppers > fct_subscription_process";
    // A width that cannot hold the full trail but can hold ".. > root".
    let max = ".. > fct_subscription_process".chars().count();
    assert!(max < full.chars().count());
    assert_eq!(
        a.breadcrumb(max).as_deref(),
        Some(".. > fct_subscription_process"),
        "overflow drops whole entries from the left, ASCII '..' prefix"
    );
    // The ".." prefix is ASCII, never the ellipsis char.
    assert!(!a.breadcrumb(max).unwrap().contains('…'));
}

#[test]
fn breadcrumb_skips_uids_that_no_longer_resolve() {
    // A vanished history uid is unconstructable via jump_to (it refuses an
    // unresolvable id), so set `back` directly — the breadcrumb must skip it.
    let mut a = app();
    a.back = vec![
        "model.jaffle_finance.ghost_does_not_exist".into(),
        "model.jaffle_finance.stg_payment__shoppers".into(),
    ];
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    assert_eq!(
        a.breadcrumb(200).as_deref(),
        Some("stg_payment__shoppers > fct_subscription_process"),
        "the unresolvable ghost uid is dropped, not rendered"
    );
}

#[test]
fn select_by_unique_id_moves_selection_and_reports_missing() {
    let mut a = app();
    let target = "model.jaffle_finance.fct_subscription_process";
    assert!(a.select_by_unique_id(target));
    assert_eq!(
        a.active_list()
            .model_at(a.ui_state.selected())
            .unwrap()
            .unique_id,
        target
    );
    assert!(!a.select_by_unique_id("model.does.not.exist"));
}

#[test]
fn reload_preserves_selection_by_unique_id() {
    let mut a = app();
    let target = "model.jaffle_finance.pos_pay";
    a.select_by_unique_id(target);
    a.reload().expect("reload ok");
    assert_eq!(
        a.active_list()
            .model_at(a.ui_state.selected())
            .unwrap()
            .unique_id,
        target,
        "selection survives reload"
    );
    assert_eq!(a.model_list.len(), 45, "reload rebuilt the full list");
}

#[test]
fn yank_mermaid_emits_a_graph_lr_diagram() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let effects = apply_action(&mut a, Action::YankMermaid).effects;
    let text = match &effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    // Fenced so a Markdown paste renders a diagram, not plain text.
    assert!(
        text.starts_with("```mermaid\ngraph LR\n"),
        "fenced Mermaid header: {text}"
    );
    assert!(text.trim_end().ends_with("```"), "closing fence: {text}");
    // The selected node, an upstream, and a downstream all appear as nodes,
    // with sanitized ids and a materialization-tagged label.
    assert!(text.contains("model_jaffle_finance_stg_payment__shoppers"));
    assert!(
        text.contains("\"stg_payment__shoppers (view) *\""),
        "selected node tagged + marked"
    );
    assert!(
        text.contains("\"int_shoppers__combined (view)\""),
        "downstream node present"
    );
    assert!(
        text.contains("\"shoppers (source)\""),
        "upstream source present"
    );
    // An edge line uses sanitized ids.
    assert!(
            text.contains("model_jaffle_finance_stg_payment__shoppers --> model_jaffle_finance_int_shoppers__combined"),
            "downstream edge present:\n{text}"
        );
}

#[test]
fn mermaid_ids_disambiguate_collisions_deterministically() {
    use super::export::mermaid_ids;
    // All three uids sanitize to `model_p_x_y`; before the per-export map the
    // diagram silently merged them into one node. Suffixes follow sorted-uid
    // order (`.` < `_`), so the same input always yields the same ids.
    let ids = mermaid_ids(["model.p_x.y", "model.p.x_y", "model.p_x_y"]);
    assert_eq!(ids["model.p.x_y"], "model_p_x_y");
    assert_eq!(ids["model.p_x.y"], "model_p_x_y_2");
    assert_eq!(ids["model.p_x_y"], "model_p_x_y_3");

    // A suffixed id must not collide with a uid that NATURALLY sanitizes to
    // that text: `a.b` takes `a_b`, so `a_b` bumps to `a_b_2`, which is
    // already claimed by `a_b_2` itself by then — everyone stays distinct.
    let ids = mermaid_ids(["a_b_2", "a.b", "a_b"]);
    let unique: BTreeSet<&String> = ids.values().collect();
    assert_eq!(unique.len(), ids.len(), "no two uids share an id: {ids:?}");

    // Reserved Mermaid words are pre-claimed — a uid sanitizing to `end`
    // would otherwise close an enclosing block. Empty input degrades to `_`.
    let ids = mermaid_ids(["end", ""]);
    assert_eq!(ids["end"], "end_2");
    assert_eq!(ids[""], "_");
}

#[test]
fn mermaid_label_neutralizes_hostile_display_names() {
    use super::export::mermaid_label;
    // A `"` would close the quoted label and let the rest inject raw Mermaid;
    // it becomes the `#quot;` entity (still renders as a quote).
    assert_eq!(
        mermaid_label(r#"evil"]; pwned["x"#),
        "evil#quot;]; pwned[#quot;x"
    );
    // Line breaks would start a new Mermaid statement (or worse, let an
    // indented ``` close the surrounding Markdown fence); all control chars
    // collapse to a space.
    assert_eq!(mermaid_label("a\nb\r\nc\td"), "a b  c d");
    // Benign names pass through untouched.
    assert_eq!(
        mermaid_label("stg_payment__shoppers"),
        "stg_payment__shoppers"
    );
}

#[test]
fn yank_dot_emits_a_digraph() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let text = match &apply_action(&mut a, Action::YankDot).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(text.starts_with("digraph lineage {"), "DOT header: {text}");
    assert!(text.contains("rankdir=LR;"));
    assert!(text.contains(
            "\"model.jaffle_finance.stg_payment__shoppers\" [label=\"stg_payment__shoppers\\n(view)\"];"
        ));
    assert!(text.contains(" -> "), "has at least one edge");
    assert!(text.trim_end().ends_with('}'));
}

#[test]
fn export_labels_neutralize_untrusted_kind() {
    use crate::manifest::{RawConfig, RawManifest, RawNode};
    use std::collections::HashMap;

    // `materialized` comes straight from the manifest; a crafted value like
    // `"]` must not close the Mermaid `["..."]` label (or the DOT quoted
    // label) and inject extra nodes/edges into the yanked diagram.
    let manifest = RawManifest {
        nodes: HashMap::from([(
            "model.p.evil".to_string(),
            RawNode {
                name: "evil".into(),
                resource_type: "model".into(),
                config: RawConfig {
                    materialized: Some("table\"] x[\"pwn".into()),
                },
                ..Default::default()
            },
        )]),
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
    };
    let mut a = App::new(Dag::build(&manifest), PathBuf::from("manifest.json"));
    assert!(a.select_by_unique_id("model.p.evil"));

    let mermaid = a.lineage_mermaid().expect("mermaid export");
    // The whole kind stays inside the one quoted label: every `"` becomes the
    // `#quot;` entity, so the `]` after it can never close the node.
    assert!(
        mermaid.contains("model_p_evil[\"evil (table#quot;] x[#quot;pwn) *\"]"),
        "kind is escaped into a single well-formed label:\n{mermaid}"
    );
    assert!(
        !mermaid.contains("x[\"pwn"),
        "raw injection payload must not survive:\n{mermaid}"
    );

    let dot = a.lineage_dot().expect("dot export");
    assert!(
        dot.contains(r#""model.p.evil" [label="evil\n(table\"] x[\"pwn)"];"#),
        "DOT kind quotes are backslash-escaped:\n{dot}"
    );
}

#[test]
fn yank_ascii_emits_the_lineage_text_diagram() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let text = match &apply_action(&mut a, Action::YankAscii).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    // The yank IS the pane's diagram: same layout, same glyphs.
    let expected = crate::layout_mode(&a.lineage_subgraph(), a.glyph_mode)
        .grid
        .to_text();
    assert_eq!(text, expected, "yank matches the rendered grid");
    assert!(
        text.contains("stg_payment__shoppers"),
        "selected name in the diagram"
    );
    // ASCII mode swaps the glyph repertoire in the yank too.
    a.glyph_mode = crate::GlyphMode::Ascii;
    let ascii = match &apply_action(&mut a, Action::YankAscii).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(ascii.is_ascii(), "ASCII-mode yank is pure ASCII");
}

#[test]
fn yank_sql_emits_the_raw_code_and_noops_without_sql() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let text = match &apply_action(&mut a, Action::YankSql).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(
        text.as_str(),
        a.dag
            .raw_code("model.jaffle_finance.stg_payment__shoppers")
            .unwrap(),
        "yank is the side-map raw SQL verbatim"
    );
    // Manifests record seeds with `raw_code: ""`; the selection-side filter
    // treats blank SQL as absent so a yank never clobbers the clipboard
    // with nothing. (Seeds are not list-selectable, so assert the side-map
    // shape the filter exists for.)
    assert_eq!(
        a.dag.raw_code("seed.jaffle_finance.fiscal_years"),
        Some(""),
        "seed raw_code is the empty string the filter guards against"
    );
}

#[test]
fn yank_impact_emits_a_markdown_blast_radius_report() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let text = match &apply_action(&mut a, Action::YankImpact).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(
        text.starts_with("# Impact: stg_payment__shoppers\n"),
        "title: {text}"
    );
    // Counts agree with the frozen impact_counts fixture values (1 down, 2 up).
    assert!(text.contains("- transitive: 2 upstream / 1 downstream\n"));
    assert!(text.contains("## Downstream (blast radius) (1)\n"));
    assert!(text.contains("## Upstream (2)\n"));
    assert!(
        text.contains("- int_shoppers__combined\n"),
        "downstream member listed: {text}"
    );
    // Deterministic: a second yank is byte-identical.
    let again = match &apply_action(&mut a, Action::YankImpact).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, again, "two yanks are byte-identical");
}

#[test]
fn yank_impact_lists_affected_exposures_with_kind_and_owner() {
    // Rooted on fct: the report splits the 2 exposures OUT of the Downstream
    // list into their own section, each line carrying kind + owner (full
    // name <email> for the dashboard; name-only for the notebook).
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let text = match &apply_action(&mut a, Action::YankImpact).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(
        text.contains("- transitive: 27 upstream / 2 downstream\n"),
        "transitive counts exclude exposures: {text}"
    );
    assert!(text.contains("- affected exposures: 2\n"), "{text}");
    assert!(
        text.contains("## Downstream (blast radius) (2)\n"),
        "{text}"
    );
    assert!(text.contains("## Affected exposures (2)\n"), "{text}");
    assert!(
        text.contains(
            "- delivery_kpi_dashboard (dashboard, owner: Finance Analytics <finance-data@example.com>)\n"
        ),
        "dashboard line with kind + full owner: {text}"
    );
    assert!(
        text.contains("- subscription_churn_notebook (notebook, owner: Analytics)\n"),
        "notebook line with name-only owner: {text}"
    );
    assert!(
        !text.contains("- subscription_churn_notebook\n"),
        "exposures never appear as bare Downstream entries: {text}"
    );
}

#[test]
fn exposures_are_cursor_reachable_but_never_re_rootable() {
    // Exposures are not list-selectable (the model list holds models only), so
    // Enter / a click on one must open its structure modal — never re-root.
    let fct = "model.jaffle_finance.fct_subscription_process";
    let exp = "exposure.jaffle_finance.subscription_churn_notebook";
    let mut a = app();
    a.select_by_unique_id(fct);
    assert!(
        !a.model_list
            .models
            .iter()
            .any(|m| m.unique_id.starts_with("exposure.")),
        "exposures never join the selectable model list"
    );
    // A lineage click on the exposure moves the CURSOR there, root unchanged.
    a.click_lineage_node(exp);
    assert_eq!(a.selected_unique_id().as_deref(), Some(fct), "no re-root");
    assert_eq!(a.lineage_cursor_uid().as_deref(), Some(exp), "cursor moved");
    // Enter on it opens the structure modal with ITS blast radius (a leaf:
    // 0 downstream, fct + fct's 27 ancestors = 28 upstream).
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.model_id, exp);
            assert_eq!(
                (dv.downstream_count, dv.upstream_count),
                (0, 28),
                "terminator leaf counts"
            );
        }
        m => panic!("expected Detail for the exposure, got {m:?}"),
    }
    assert_eq!(a.selected_unique_id().as_deref(), Some(fct), "still rooted");
}

#[test]
fn yank_selector_mirrors_the_lineage_view_as_graph_operators() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let yank = |a: &mut App| match &apply_action(a, Action::YankSelector).effects[..] {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    // Default view (both directions, unlimited) = the full closure.
    assert_eq!(yank(&mut a), "dbt build --select +stg_payment__shoppers+");
    // Downstream toggled off → upstream-only prefix form.
    apply_action(&mut a, Action::ToggleDownstream);
    assert_eq!(yank(&mut a), "dbt build --select +stg_payment__shoppers");
    // Upstream off too → just the node itself.
    apply_action(&mut a, Action::ToggleUpstream);
    assert_eq!(yank(&mut a), "dbt build --select stg_payment__shoppers");
    // Downstream back on → suffix form.
    apply_action(&mut a, Action::ToggleDownstream);
    assert_eq!(yank(&mut a), "dbt build --select stg_payment__shoppers+");
    // A depth limit bounds the active side(s): `[` enters at 3 hops.
    apply_action(&mut a, Action::DepthDecrease);
    assert_eq!(yank(&mut a), "dbt build --select stg_payment__shoppers+3");
    apply_action(&mut a, Action::ToggleUpstream);
    assert_eq!(yank(&mut a), "dbt build --select 3+stg_payment__shoppers+3");
    // Reset restores the default form; the toast records the copy intent.
    apply_action(&mut a, Action::ResetView);
    assert_eq!(yank(&mut a), "dbt build --select +stg_payment__shoppers+");
    assert_eq!(a.take_notice().as_deref(), Some("Copied dbt selector"));
}

#[test]
fn export_lineage_emits_a_write_file_effect_with_the_diagram() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let effects = apply_action(&mut a, Action::ExportLineage).effects;
    let (path, contents) = match &effects[..] {
        [Effect::WriteFile { path, contents }] => (path.clone(), contents.clone()),
        other => panic!("expected one WriteFile effect, got {other:?}"),
    };
    assert_eq!(path, "stg_payment__shoppers_lineage.txt");
    assert_eq!(
        contents,
        a.lineage_ascii().unwrap(),
        "exported contents are the lineage text diagram"
    );
}

#[test]
fn export_sanitizes_untrusted_node_names_into_a_safe_relative_path() {
    // Node names come from an untrusted manifest.json: a name carrying path
    // separators must not let the export escape the working directory.
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    nodes.insert(
        "model.p.evil".to_string(),
        RawNode {
            name: "../../evil".into(),
            resource_type: "model".into(),
            path: Some("marts/evil.sql".into()),
            ..Default::default()
        },
    );
    let dag = Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
    });
    let mut a = App::new(dag, PathBuf::from("/tmp/x/target/manifest.json"));
    a.select_by_unique_id("model.p.evil");
    let effects = apply_action(&mut a, Action::ExportLineage).effects;
    let path = match &effects[..] {
        [Effect::WriteFile { path, .. }] => path.clone(),
        other => panic!("expected one WriteFile effect, got {other:?}"),
    };
    assert_eq!(path, ".._.._evil_lineage.txt");
    let p = Path::new(&path);
    assert!(p.is_relative(), "export path stays relative");
    assert_eq!(
        p.components().count(),
        1,
        "a single component: separators cannot traverse out of the CWD"
    );
}

#[test]
fn page_down_and_up_move_the_list_selection_by_ten_clamped() {
    let mut a = app();
    assert_eq!(a.ui_state.selected(), 0);
    apply_action(&mut a, Action::PageDown);
    assert_eq!(a.ui_state.selected(), 10, "page down = +10");
    apply_action(&mut a, Action::PageUp);
    assert_eq!(a.ui_state.selected(), 0, "page up = -10");
    apply_action(&mut a, Action::PageUp);
    assert_eq!(a.ui_state.selected(), 0, "clamped at the top");
    for _ in 0..20 {
        apply_action(&mut a, Action::PageDown);
    }
    assert_eq!(
        a.ui_state.selected(),
        a.model_list.len() - 1,
        "clamped at the bottom"
    );
}

#[test]
fn modal_page_scroll_and_home_end_record_intent() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    apply_action(&mut a, Action::SqlOpen);
    assert!(matches!(a.mode, Mode::Sql(_)), "SQL modal open");
    apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
    let scroll = |m: &Mode| match m {
        Mode::Sql(sv) => sv.scroll,
        other => panic!("expected Sql mode, got {other:?}"),
    };
    assert_eq!(scroll(&a.mode), 10, "page = 10 lines");
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    assert_eq!(scroll(&a.mode), 11, "line scroll still 1");
    apply_action(&mut a, Action::DetailScrollPage(Direction::Up));
    assert_eq!(scroll(&a.mode), 1, "page up = -10 saturating");
    apply_action(&mut a, Action::DetailScrollEnd);
    assert_eq!(
        scroll(&a.mode),
        usize::MAX,
        "End records MAX for the loop clamp"
    );
    apply_action(&mut a, Action::DetailScrollHome);
    assert_eq!(scroll(&a.mode), 0, "Home rewinds to the top");
    // The same arms drive the help overlay.
    apply_action(&mut a, Action::DetailClose);
    apply_action(&mut a, Action::HelpToggle);
    apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
    assert!(
        matches!(a.mode, Mode::Help { scroll: 10 }),
        "help pages too"
    );
}

#[test]
fn gap_next_and_prev_cycle_through_untested_models() {
    // The big fixture has zero untested MODELS, so it pins the no-op side;
    // the sample manifest (dim_customers / agg_country_orders /
    // fct_customer_history untested) exercises the cycle itself.
    let mut full = app();
    apply_action(&mut full, Action::GapNext);
    assert_eq!(full.ui_state.selected(), 0, "no gaps -> selection unmoved");

    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let mut a = App::new(
        load_dag(sample).expect("sample loads"),
        PathBuf::from(sample),
    );
    let gaps: Vec<usize> = a
        .model_list
        .models
        .iter()
        .enumerate()
        .filter(|(_, m)| crate::coverage_gap(m))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(gaps.len(), 3, "sample manifest has three untested models");
    apply_action(&mut a, Action::GapNext);
    let first = a.ui_state.selected();
    assert!(gaps.contains(&first), "n lands on a coverage gap");
    apply_action(&mut a, Action::GapNext);
    let second = a.ui_state.selected();
    assert!(gaps.contains(&second) && second != first, "n cycles onward");
    apply_action(&mut a, Action::GapPrev);
    assert_eq!(a.ui_state.selected(), first, "N walks back");
}

#[test]
fn bookmark_cycle_back_walks_bookmarks_in_reverse() {
    let mut a = app();
    // Bookmark models at indices 3 and 7, then cycle backward from 0.
    for i in [3usize, 7] {
        a.ui_state.set_selected(i);
        apply_action(&mut a, Action::BookmarkToggle);
    }
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::BookmarkCycleBack);
    assert_eq!(
        a.ui_state.selected(),
        7,
        "backward wraps to the last bookmark"
    );
    apply_action(&mut a, Action::BookmarkCycleBack);
    assert_eq!(
        a.ui_state.selected(),
        3,
        "backward again hits the earlier one"
    );
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(a.ui_state.selected(), 7, "forward cycles onward");
}

#[test]
fn lineage_extreme_jumps_send_the_cursor_to_first_and_last_columns() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let lay = crate::layout(&a.lineage_subgraph());
    let max_col = lay.columns.values().max().copied().unwrap();
    assert!(max_col >= 2, "fixture lineage spans 3+ columns");
    apply_action(&mut a, Action::LineageRightmost);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&cur], max_col, "L lands in the last column");
    apply_action(&mut a, Action::LineageLeftmost);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&cur], 0, "H lands in the first column");
}

#[test]
fn untested_filter_narrows_the_list_and_toggles_off() {
    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let mut a = App::new(
        load_dag(sample).expect("sample loads"),
        PathBuf::from(sample),
    );
    let full = a.model_list.len();
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::Untested);
    assert_eq!(a.active_list().len(), 3, "only the three untested models");
    assert!(
        a.active_list().models.iter().all(crate::coverage_gap),
        "every visible model is untested"
    );
    assert_eq!(
        a.ui_state.model_count(),
        3,
        "selection space follows the filtered view"
    );
    assert_eq!(a.list_filter_label(), Some("untested"));
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::All, "second press toggles off");
    assert_eq!(a.active_list().len(), full, "full list restored");
    assert_eq!(a.list_filter_label(), None);
}

#[test]
fn bookmark_filter_tracks_toggles_and_search_narrows_on_top() {
    let mut a = app();
    let kept = "model.jaffle_finance.stg_payment__shoppers";
    a.select_by_unique_id(kept);
    apply_action(&mut a, Action::BookmarkToggle);
    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert_eq!(a.active_list().len(), 1, "only the bookmarked model");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(kept),
        "selection re-resolved by id into the filtered view"
    );
    // A list search narrows FROM the bookmarked view: a query matching
    // many full-list models still shows only the bookmarked one.
    apply_action(&mut a, Action::SearchOpen);
    for c in "stg".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    assert_eq!(a.active_list().len(), 1, "search composes with the filter");
    apply_action(&mut a, Action::SearchCancel);
    // Un-bookmarking the row under the live Bookmarked filter empties it.
    apply_action(&mut a, Action::BookmarkToggle);
    assert_eq!(a.active_list().len(), 0, "un-bookmark leaves the view");
    // Toggling the OTHER filter replaces, not stacks.
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::Untested);
    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert_eq!(a.list_filter, ListFilter::Bookmarked);
}

#[test]
fn critical_path_is_a_deterministic_source_rooted_chain() {
    let a = app();
    let sv = a.compute_stats_view();
    assert!(
        sv.critical_path.len() >= 4,
        "the fixture graph is at least 4 deep, got {:?}",
        sv.critical_path
    );
    // The chain start must have no parents (else its parent's chain would
    // be longer) and the end no children — both checked by name lookup.
    let by_name = |name: &str| {
        a.dag
            .nodes()
            .values()
            .filter(|n| n.name == name)
            .collect::<Vec<_>>()
    };
    let first = sv.critical_path.first().unwrap();
    assert!(
        by_name(first).iter().any(|n| n.direct_up == 0),
        "chain starts at a root: {first}"
    );
    let last = sv.critical_path.last().unwrap();
    assert!(
        by_name(last).iter().any(|n| n.direct_down == 0),
        "chain ends at a leaf: {last}"
    );
    // Deterministic: a second computation is identical.
    assert_eq!(sv.critical_path, a.compute_stats_view().critical_path);
}

#[test]
fn longest_chain_terminates_on_a_cyclic_manifest() {
    // A malformed manifest.json can carry a model-level cycle that survives
    // the prune (only test/operation nodes are dropped): a ⇄ b, plus a clean
    // tail b → c. Without the on-stack guard the memoized DFS recursed
    // forever and overflowed the stack as soon as the Stats dashboard (`i`)
    // computed the critical path.
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    let mut add = |id: &str, name: &str| {
        nodes.insert(
            id.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                ..Default::default()
            },
        );
    };
    add("model.p.a", "a");
    add("model.p.b", "b");
    add("model.p.c", "c");
    let child_map = HashMap::from([
        ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
        (
            "model.p.b".to_string(),
            vec!["model.p.a".to_string(), "model.p.c".to_string()],
        ),
    ]);
    let parent_map = HashMap::from([
        ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
        ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
        ("model.p.c".to_string(), vec!["model.p.b".to_string()]),
    ]);
    let dag = Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map,
        child_map,
    });
    let chain = super::analysis::longest_chain(&dag);
    // Sane: non-empty, no repeated node, every hop is a real kept edge.
    assert!(!chain.is_empty(), "a chain is still produced");
    let unique: std::collections::HashSet<&String> = chain.iter().collect();
    assert_eq!(unique.len(), chain.len(), "the chain never revisits a node");
    let edges: std::collections::HashSet<(String, String)> = dag
        .edges()
        .into_iter()
        .map(|(p, c)| {
            let name = |uid: &str| {
                dag.get(uid)
                    .map_or_else(|| uid.to_string(), |n| n.name.clone())
            };
            (name(&p), name(&c))
        })
        .collect();
    for hop in chain.windows(2) {
        assert!(
            edges.contains(&(hop[0].clone(), hop[1].clone())),
            "every hop is a real edge: {hop:?}"
        );
    }
    // Deterministic, smallest-uid-first: the DFS visits `a` first, so the
    // cycle breaks at the back-edge into `a` and the chain is a → b.
    assert_eq!(chain, vec!["a".to_string(), "b".to_string()]);
    // Deterministic: a second computation is identical.
    assert_eq!(chain, super::analysis::longest_chain(&dag));
}

#[test]
fn sql_modal_payload_carries_the_file_path() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(
            sv.path.as_deref(),
            Some("models/utilities/pos_prep/pos_txn.sql"),
            "path snapshotted at open"
        ),
        other => panic!("expected Sql mode, got {other:?}"),
    }
}

#[test]
fn select_by_name_resolves_models_and_rejects_unknowns() {
    let mut a = app();
    assert!(a.select_by_name("pos_txn"), "known model name selects");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.jaffle_finance.pos_txn")
    );
    let before = a.ui_state.selected();
    assert!(!a.select_by_name("no_such_model"), "unknown name rejected");
    assert_eq!(a.ui_state.selected(), before, "selection untouched");
    // The watch root in manifest mode is the manifest FILE.
    let (root, recursive) = a.watch_root();
    assert!(root.ends_with("manifest.json"));
    assert!(!recursive);
}

#[test]
fn jump_to_records_history_and_back_forward_navigate() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let start = a.selected_unique_id().unwrap();
    a.jump_to("model.jaffle_finance.int_shoppers__combined");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.jaffle_finance.int_shoppers__combined")
    );
    a.history_back();
    assert_eq!(
        a.selected_unique_id(),
        Some(start),
        "back returns to the origin"
    );
    a.history_forward();
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.jaffle_finance.int_shoppers__combined"),
        "forward re-applies the jump"
    );
    // A new jump clears the forward stack.
    a.history_back();
    a.jump_to("model.jaffle_finance.pos_txn");
    a.history_forward();
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.jaffle_finance.pos_txn"),
        "forward is a no-op after a new jump cleared it"
    );
}

#[test]
fn reload_prunes_vanished_ids_from_history_and_back_lands_on_a_survivor() {
    let mut a = app();
    let survivor = "model.jaffle_finance.stg_payment__shoppers";
    let current = "model.jaffle_finance.fct_subscription_process";
    a.select_by_unique_id(survivor);
    a.jump_to("model.jaffle_finance.int_shoppers__combined");
    a.jump_to(current);
    // Simulate visited nodes that the reload removes: a vanished uid is
    // unconstructable via jump_to (it refuses an unresolvable id), so plant
    // ghosts directly — the fixture re-read won't contain them. The back
    // stack sandwiches a ghost between equal survivors (A → ghost → A), the
    // post-prune adjacent-duplicate case.
    a.back = vec![
        survivor.into(),
        "model.jaffle_finance.ghost_does_not_exist".into(),
        survivor.into(),
    ];
    a.forward = vec!["model.jaffle_finance.ghost_ahead".into()];
    a.reload().expect("reload ok");
    assert_eq!(
        a.back,
        vec![survivor.to_string()],
        "back keeps survivors only, adjacent duplicates collapsed"
    );
    assert!(
        a.forward.is_empty(),
        "the vanished forward entry is dropped"
    );
    a.history_back();
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(survivor),
        "back lands on the most recent surviving entry, not a no-op"
    );
    assert_eq!(
        a.forward,
        vec![current.to_string()],
        "forward holds exactly the node back navigated away from"
    );
}

#[test]
fn lineage_view_toggles_and_depth_filter_the_subgraph() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let full = a.lineage_subgraph().nodes.len();
    assert_eq!(
        full, 32,
        "full lineage = 27 up + selected + 2 down + 2 exposures"
    );

    // Upstream-only: drop the downstream side.
    apply_action(&mut a, Action::ToggleDownstream);
    let up_only = a.lineage_subgraph();
    assert_eq!(up_only.nodes.len(), 28, "27 upstream + selected");
    assert!(
        !up_only
            .nodes
            .iter()
            .any(|n| n.unique_id == "model.jaffle_finance.fct_delivery_monthly_snapshot"),
        "a downstream node is excluded"
    );

    // Reset restores the full view.
    apply_action(&mut a, Action::ResetView);
    assert_eq!(a.lineage_subgraph().nodes.len(), full);

    // Depth limit: None → 3 → 2 → 1, shrinking the neighbourhood.
    apply_action(&mut a, Action::DepthDecrease);
    apply_action(&mut a, Action::DepthDecrease);
    apply_action(&mut a, Action::DepthDecrease);
    assert_eq!(a.lineage_view.depth, Some(1));
    let d1 = a.lineage_subgraph().nodes.len();
    assert!(
        d1 > 1 && d1 < full,
        "1-hop neighbourhood is a strict subset: {d1}"
    );
    // Widening past the cap returns to unlimited.
    for _ in 0..10 {
        apply_action(&mut a, Action::DepthIncrease);
    }
    assert_eq!(
        a.lineage_view.depth, None,
        "widening past 8 hops → unlimited"
    );
    assert_eq!(a.lineage_subgraph().nodes.len(), full);
}

#[test]
fn reload_error_leaves_state_unchanged() {
    // A missing manifest path: reload returns Err BEFORE mutating, so the app
    // keeps running on the old data (run_effect swallows the Err).
    let dag = load_dag(FIXTURE).expect("fixture loads");
    let mut a = App::new(dag, PathBuf::from("/no/such/manifest.json"));
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let before_len = a.model_list.len();
    let before_sel = a.selected_unique_id();
    assert!(a.reload().is_err(), "missing manifest → Err");
    assert_eq!(
        a.model_list.len(),
        before_len,
        "list unchanged on reload error"
    );
    assert_eq!(
        a.selected_unique_id(),
        before_sel,
        "selection unchanged on reload error"
    );
}

#[test]
fn dbt_parse_action_requests_the_effect_without_a_notice() {
    // The reducer records intent only — PATH lookup, the subprocess, and
    // the outcome notice all belong to `main::run_dbt_parse`.
    let mut a = app();
    let out = apply_action(&mut a, Action::DbtParse);
    assert_eq!(out.effects, vec![Effect::DbtParse]);
    assert!(!out.quit);
    assert_eq!(a.take_notice(), None, "no optimistic toast for dbt parse");
}

#[test]
fn adopt_manifest_switches_the_source_and_reverts_on_error() {
    let project = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_project");
    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let dag = load_dag_from_source(project).expect("sample project loads");
    let mut a = App::from_source(dag, PathBuf::from(project));
    assert!(a.is_source_mode());
    assert_eq!(a.project_root(), Path::new(project));

    // A bad manifest path: Err, and the previous source survives (the
    // never-corrupt contract) — reload still re-reads the project.
    a.select_by_unique_id("model.sample.stg_orders");
    assert!(a
        .adopt_manifest(PathBuf::from("/no/such/manifest.json"))
        .is_err());
    assert!(a.is_source_mode(), "failed adopt restores the source");
    assert_eq!(
        a.project_root(),
        Path::new(project),
        "failed adopt keeps the project root"
    );
    assert!(a.reload().is_ok(), "the restored source still loads");

    // The real manifest: source flips, watch follows the FILE, and the
    // selection survives by unique_id across the rebuild.
    a.select_by_unique_id("model.sample.stg_orders");
    a.adopt_manifest(PathBuf::from(sample)).expect("adopts");
    assert!(!a.is_source_mode());
    let (root, recursive) = a.watch_root();
    assert!(root.ends_with("sample_manifest.json"));
    assert!(!recursive);
    assert_eq!(
        a.project_root(),
        // derive_project_root strips `<root>/target/manifest.json` → `<root>`
        // (two parents); same derivation `App::new` would have applied.
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")),
        "project root re-derived from the adopted manifest"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.stg_orders"),
        "selection restored by id after the swap"
    );
}

#[test]
fn effect_actions_emit_effects_without_mutating_state() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let before = a.ui_state.selected();

    // Yank id / name produce a clipboard effect carrying the right text.
    assert_eq!(
        apply_action(&mut a, Action::YankId).effects,
        vec![Effect::Yank("model.jaffle_finance.pos_txn".into())]
    );
    assert_eq!(
        apply_action(&mut a, Action::YankName).effects,
        vec![Effect::Yank("pos_txn".into())]
    );
    // Reload always requests a reload effect.
    assert_eq!(
        apply_action(&mut a, Action::Reload).effects,
        vec![Effect::ReloadManifest]
    );
    // OpenEditor resolves the SQL path under the project root.
    match &apply_action(&mut a, Action::OpenEditor).effects[..] {
        [Effect::OpenEditor(path)] => {
            assert!(
                path.ends_with("models/utilities/pos_prep/pos_txn.sql"),
                "got {path}"
            );
        }
        other => panic!("expected one OpenEditor effect, got {other:?}"),
    }
    // The pure reducer never mutated selection while emitting effects.
    assert_eq!(a.ui_state.selected(), before);
}

#[test]
fn derive_project_root_strips_target() {
    let root = derive_project_root(Path::new("/x/proj/target/manifest.json"));
    assert_eq!(root, PathBuf::from("/x/proj"));
}

#[test]
fn selected_file_path_stays_under_the_project_root() {
    // `original_file_path` comes from an untrusted manifest.json: a traversal
    // value must yield None (no editor jump), while plain relative paths keep
    // resolving under the root exactly as before.
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;

    let node = |name: &str, ofp: &str| RawNode {
        name: name.to_string(),
        resource_type: "model".to_string(),
        original_file_path: Some(ofp.to_string()),
        ..Default::default()
    };
    let mut nodes = HashMap::new();
    nodes.insert("model.p.ok".to_string(), node("ok", "models/ok.sql"));
    nodes.insert(
        "model.p.dotted".to_string(),
        node("dotted", "./models/dotted.sql"),
    );
    nodes.insert(
        "model.p.up".to_string(),
        node("up", "../../../../home/user/.bashrc"),
    );
    nodes.insert("model.p.abs".to_string(), node("abs", "/etc/passwd"));
    nodes.insert(
        "model.p.mid".to_string(),
        node("mid", "models/../../outside.sql"),
    );
    let dag = Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
    });
    let mut a = App::new(dag, PathBuf::from("/x/proj/target/manifest.json"));

    a.select_by_unique_id("model.p.ok");
    // Built with the same `join` as the implementation so the expected string
    // carries the platform separator (Windows joins with `\`).
    let expected = Path::new("/x/proj")
        .join("models/ok.sql")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        a.selected_file_path().as_deref(),
        Some(expected.as_str()),
        "a normal relative path resolves under the root, unchanged"
    );
    a.select_by_unique_id("model.p.dotted");
    let dotted = a
        .selected_file_path()
        .expect("a ./-prefixed path is benign");
    assert!(dotted.ends_with("models/dotted.sql"), "got {dotted}");

    for uid in ["model.p.up", "model.p.abs", "model.p.mid"] {
        a.select_by_unique_id(uid);
        assert_eq!(
            a.selected_file_path(),
            None,
            "{uid} must not resolve outside the project root"
        );
    }
}

#[test]
fn lineage_styles_maps_materialization_and_tests() {
    let a = app();
    let lay = crate::layout(&a.dag.subgraph("model.jaffle_finance.pos_txn"));
    let styles = a.lineage_styles(&lay);
    // pos_txn is a table model.
    assert_eq!(
        styles.get("model.jaffle_finance.pos_txn").unwrap().class,
        MaterializationClass::Table
    );
    // Its upstream subgraph contains source/seed nodes, classed accordingly.
    assert!(
        styles
            .values()
            .any(|c| c.class == MaterializationClass::Source),
        "a source node is classed Source"
    );
    assert!(
        styles
            .values()
            .any(|c| c.class == MaterializationClass::Seed),
        "a seed node is classed Seed"
    );
}

#[test]
fn help_toggle_opens_and_closes() {
    let mut a = app();
    apply_action(&mut a, Action::HelpToggle);
    assert!(matches!(a.mode, Mode::Help { .. }), "? opens help");
    apply_action(&mut a, Action::HelpToggle);
    assert!(matches!(a.mode, Mode::Selection), "? again closes help");
}

#[test]
fn lineage_search_jumps_to_a_matching_upstream_node() {
    // Open a lineage-target search from the RIGHT pane and confirm: a matching
    // UPSTREAM model becomes the new selection (re-root).
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::SearchOpen);
    assert!(
        matches!(&a.mode, Mode::Search(s) if s.target == SearchTarget::Lineage),
        "search from the lineage pane targets the lineage"
    );
    for c in "dimsupp".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    // "dimsupp" matches a dim_supplier* model — an upstream of fct.
    let fct = "model.jaffle_finance.fct_subscription_process";
    let hit = a.current_lineage_match().expect("a lineage node matches");
    assert!(
        a.dag.upstream(fct).contains(&hit),
        "the match is an upstream of fct: {hit}"
    );
    assert_ne!(
        hit, fct,
        "the match is a different node than the current root"
    );
    apply_action(&mut a, Action::SearchConfirm);
    assert!(matches!(a.mode, Mode::Selection), "confirm leaves search");
    assert_eq!(
        a.active_list()
            .model_at(a.ui_state.selected())
            .unwrap()
            .unique_id,
        hit,
        "the matched upstream model is now selected (re-rooted)"
    );
}

// ---- lineage cursor: spatial movement, focus routing, Enter commit ----

const FCT: &str = "model.jaffle_finance.fct_subscription_process";

#[test]
fn lineage_cursor_moves_spatially_under_right_pane_focus() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    let lay = crate::layout(&a.lineage_subgraph());
    let root_col = lay.columns[FCT];
    assert!(root_col > 0, "fct has upstream columns");

    // h: exactly one column upstream per keypress, down to column 0.
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&cur], root_col - 1, "h = one column left");
    for expect in (0..root_col - 1).rev() {
        apply_action(&mut a, Action::MoveLeft);
        let c = a.lineage_cursor_uid().unwrap();
        assert_eq!(lay.columns[&c], expect, "each h steps one column");
    }
    let edge = a.lineage_cursor_uid().unwrap();
    apply_action(&mut a, Action::MoveLeft);
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(edge.as_str()),
        "h at the most-upstream column is a no-op"
    );

    // j/k: step through column 0's stack (sources/seeds — always ≥2 for
    // fct) and round-trip.
    let mut col: Vec<(String, usize)> = lay
        .rects
        .iter()
        .filter(|(uid, _)| lay.columns[uid.as_str()] == 0)
        .map(|(uid, r)| (uid.clone(), r.y))
        .collect();
    col.sort_by_key(|(_, y)| *y);
    assert!(col.len() >= 2, "fixture: column 0 stacks nodes");
    let i = col.iter().position(|(uid, _)| *uid == edge).unwrap();
    if i + 1 < col.len() {
        apply_action(&mut a, Action::MoveDown);
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(col[i + 1].0.as_str()),
            "j = next node down the column"
        );
        apply_action(&mut a, Action::MoveUp);
    } else {
        apply_action(&mut a, Action::MoveUp);
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(col[i - 1].0.as_str()),
            "k = previous node up the column"
        );
        apply_action(&mut a, Action::MoveDown);
    }
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(edge.as_str()),
        "j/k round-trips"
    );

    // l: one column back toward downstream.
    apply_action(&mut a, Action::MoveRight);
    let back = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&back], 1, "l = one column right");

    // The rooted selection never moved while the cursor walked.
    assert_eq!(a.selected_unique_id().as_deref(), Some(FCT));
}

#[test]
fn lineage_cursor_routing_display_subgraph_and_reset() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    // List focus: movement keys drive the LIST selection; the cursor stays home.
    let before = a.ui_state.selected();
    apply_action(&mut a, Action::MoveDown);
    assert_eq!(
        a.ui_state.selected(),
        before + 1,
        "list focus moves the list"
    );
    assert_eq!(
        a.lineage_cursor_uid(),
        a.selected_unique_id(),
        "cursor home = the selection"
    );

    // Lineage focus: the cursor walks and the DISPLAY subgraph re-selects it…
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur.as_str(), FCT, "the cursor left the root");
    assert_eq!(
        a.lineage_display_subgraph().selected,
        cur,
        "display subgraph selects (→ emphasizes, anchors) the cursor"
    );
    // …while ROOT semantics (exports, matches, title) stay on the selection.
    assert_eq!(a.lineage_subgraph().selected, FCT);

    // z (recenter) sends the cursor home.
    apply_action(&mut a, Action::Recenter);
    assert_eq!(a.lineage_cursor_uid().as_deref(), Some(FCT));
}

#[test]
fn toggle_list_pane_pins_focus_and_movement_drives_the_lineage_cursor() {
    let mut a = app();
    a.select_by_unique_id(FCT);

    // Hide the list: focus is pinned to the lineage pane…
    apply_action(&mut a, Action::ToggleListPane);
    assert!(!a.ui_state.list_visible());
    assert_eq!(a.ui_state.focus(), Focus::RightPane);
    // …so the movement keys route to the lineage CURSOR, never the hidden
    // list (the selection — the lineage root — must not move).
    apply_action(&mut a, Action::MoveLeft);
    assert_ne!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "h walks the cursor upstream while the list is hidden"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "the rooted selection stays put"
    );

    // Show it again: the list pane takes focus back.
    apply_action(&mut a, Action::ToggleListPane);
    assert!(a.ui_state.list_visible());
    assert_eq!(a.ui_state.focus(), Focus::List);
}

#[test]
fn stale_lineage_cursor_falls_back_to_the_root() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft); // cursor = an upstream node
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur.as_str(), FCT);
    // Dropping the upstream side removes the cursor's node from the subgraph.
    apply_action(&mut a, Action::ToggleUpstream);
    assert!(
        !a.lineage_subgraph().contains(&cur),
        "precondition: the cursor's node was dropped from the view"
    );
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "a stale cursor falls back to the root"
    );
    assert_eq!(a.lineage_display_subgraph().selected, FCT);
}

#[test]
fn reload_sends_the_cursor_home() {
    // Reload restores the selection BY ID, so the loop's selection-change
    // chokepoint never fires — reload itself must re-home the cursor or a
    // pre-reload cursor would survive into the rebuilt graph.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    assert_ne!(a.lineage_cursor_uid().as_deref(), Some(FCT));
    a.reload().expect("reload ok");
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "reload re-homes the cursor to the (restored) root"
    );
}

#[test]
fn click_reroots_models_moves_cursor_for_sources_and_rehomes_on_root() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    // Click an upstream MODEL: re-root + cursor home.
    let up_model = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("model."))
        .min()
        .cloned()
        .unwrap();
    a.click_lineage_node(&up_model);
    assert_eq!(a.selected_unique_id().as_deref(), Some(up_model.as_str()));
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(up_model.as_str()),
        "cursor is home on the new root"
    );

    // Click a SOURCE: no re-root, the CURSOR moves there.
    a.select_by_unique_id(FCT);
    let src = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("source."))
        .min()
        .cloned()
        .unwrap();
    a.click_lineage_node(&src);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "clicking a source never re-roots"
    );
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(src.as_str()),
        "the cursor moved to the clicked source"
    );

    // Click the CURRENT ROOT: id-preserving, so the loop's chokepoint won't
    // fire — the click itself must send the cursor home.
    a.click_lineage_node(FCT);
    assert_eq!(a.selected_unique_id().as_deref(), Some(FCT));
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "clicking the root re-homes the cursor"
    );
}

#[test]
fn enter_commits_cursor_reroot_for_models_structure_for_sources() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);

    // (a) cursor == root: Enter opens the root's structure (unchanged).
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.model_id, FCT),
        m => panic!("expected Detail for the root, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);

    // (b) cursor on an upstream MODEL: Enter re-roots (same as a click),
    // recording history.
    let up_model = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("model."))
        .min()
        .cloned()
        .expect("fct has an upstream model");
    a.lineage_cursor = Some(up_model.clone());
    apply_action(&mut a, Action::DetailOpen);
    assert!(
        matches!(a.mode, Mode::Selection),
        "a re-root stays in Selection mode"
    );
    assert_eq!(a.selected_unique_id().as_deref(), Some(up_model.as_str()));
    a.history_back();
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "the Enter re-root recorded history (b returns)"
    );

    // (c) cursor on a SOURCE (not list-selectable): Enter opens ITS
    // structure modal instead, and the root stays put.
    a.select_by_unique_id(FCT);
    let src = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("source."))
        .min()
        .cloned()
        .expect("fct has an upstream source");
    a.lineage_cursor = Some(src.clone());
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.model_id, src);
            assert_eq!(
                Some(dv.name.as_str()),
                a.dag.get(&src).map(|n| n.name.as_str()),
                "the modal is titled with the source's name"
            );
        }
        m => panic!("expected Detail for the source, got {m:?}"),
    }
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "a non-model cursor never re-roots"
    );
}

#[test]
fn detail_open_snapshots_payload_from_side_maps() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.model_id, "model.jaffle_finance.pos_txn");
            assert_eq!(dv.name, "pos_txn");
            assert_eq!(dv.detail.materialized.as_deref(), Some("table"));
            assert!(
                !dv.detail.columns.is_empty(),
                "columns came from the Dag side map"
            );
        }
        _ => panic!("expected Detail mode"),
    }
    apply_action(&mut a, Action::DetailClose);
    assert!(
        matches!(a.mode, Mode::Selection),
        "Esc/close returns to Selection"
    );
}

#[test]
fn selected_status_note_reports_materialization_and_tests() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let note = a.selected_status_note().expect("a model is selected");
    assert!(
        note.contains("table"),
        "status note names the materialization: {note}"
    );
    assert!(
        note.contains("tests:"),
        "status note includes a tests count: {note}"
    );
}

// ---- SQL preview / stats dashboard modals ----

#[test]
fn sql_open_snapshots_raw_code() {
    // A manifest-loaded model carries raw_code; SqlOpen snapshots it into the
    // Mode payload (no Dag in the render layer). pos_txn has clean ASCII SQL.
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => {
            assert_eq!(sv.model_id, "model.jaffle_finance.pos_txn");
            assert_eq!(sv.name, "pos_txn");
            assert!(
                !sv.sql.is_empty() && !sv.sql.starts_with("(no SQL"),
                "real raw_code was snapshotted: {:?}",
                &sv.sql[..sv.sql.len().min(40)]
            );
            assert_eq!(sv.scroll, 0);
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);
    assert!(matches!(a.mode, Mode::Selection), "DetailClose closes Sql");
}

#[test]
fn sql_open_source_shows_placeholder() {
    // A source has no raw_code → the modal shows a placeholder, not a panic.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    // Move the cursor onto an upstream source.
    let src = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("source."))
        .min()
        .cloned()
        .expect("fct has an upstream source");
    a.lineage_cursor = Some(src.clone());
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => {
            assert_eq!(sv.model_id, src);
            assert!(
                sv.sql.starts_with("(no SQL"),
                "source SQL is a placeholder: {}",
                sv.sql
            );
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
}

#[test]
fn sql_open_respects_focus() {
    // Lineage focus previews the CURSOR; list focus previews the selection.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    let up_model = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("model."))
        .min()
        .cloned()
        .expect("fct has an upstream model");
    a.lineage_cursor = Some(up_model.clone());
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.model_id, up_model, "lineage focus previews the cursor"),
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);

    // List focus: the cursor is ignored; the selection (the root) is previewed.
    a.ui_state.set_focus(Focus::List);
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.model_id, FCT, "list focus previews the selection"),
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
}

#[test]
fn stats_open_computes_dashboard() {
    let mut a = app();
    apply_action(&mut a, Action::StatsOpen);
    let sv = match &a.mode {
        Mode::Stats(sv) => sv.clone(),
        m => panic!("expected Mode::Stats, got {m:?}"),
    };
    // Coverage base == coverage_summary's (model|seed|snapshot): 45+7+1 = 53,
    // and the sole gap in the fixture is the untested snapshot.
    assert_eq!(
        sv.testable_total, 53,
        "fixture: 45 models + 7 seeds + 1 snapshot"
    );
    assert_eq!(
        sv.untested_testable, 1,
        "fixture: only the snapshot is untested"
    );
    assert_eq!(
        (sv.testable_tested, sv.testable_total),
        a.coverage_summary(),
        "dashboard coverage == the lens/status coverage_summary (single source)"
    );
    let rt = |k: &str| {
        sv.by_resource_type
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, n)| *n)
    };
    assert_eq!(rt("source"), Some(38), "fixture: 38 sources");
    assert_eq!(rt("seed"), Some(7), "fixture: 7 seeds");
    assert_eq!(rt("snapshot"), Some(1), "fixture: 1 snapshot");
    assert_eq!(
        sv.testable_tested + sv.untested_testable,
        sv.testable_total,
        "tested + untested == testable_total"
    );
    assert!(sv.top_hubs.len() <= 5, "at most 5 hubs");
    // Hubs are degree-desc, then unique_id-asc — verify the ordering.
    for w in sv.top_hubs.windows(2) {
        assert!(
            w[0].2 > w[1].2 || (w[0].2 == w[1].2 && w[0].0 <= w[1].0),
            "hubs sorted by degree desc then unique_id asc"
        );
    }

    // --- transitive_hubs: top-5 by downstream-closure size over ALL nodes,
    // count desc then unique_id asc (the seed/source siblings tie at 17, so
    // `seed.* < source.*` and the four pos_* payment sources order by name;
    // pos_shp @17 drops off the cap). Derived by running against the fixture
    // and verified against `Dag::downstream`, not guessed. The leader counts
    // the fixture's two exposures (both downstream of fct, which it feeds):
    // the Dag closure is the raw graph truth, unlike the chip's split counts.
    assert_eq!(
        sv.transitive_hubs,
        vec![
            ("source_datetime_policy".to_string(), 22),
            ("pos_prod_aws_store_master".to_string(), 17),
            ("pos_cat".to_string(), 17),
            ("pos_pay".to_string(), 17),
            ("pos_rcv".to_string(), 17),
        ],
        "top-5 transitive blast-radius hubs (count desc, unique_id tie-break)"
    );
    // Cross-check the leader against the live closure (reuse, not a magic number).
    assert_eq!(
        a.dag
            .downstream("seed.jaffle_finance.source_datetime_policy")
            .len(),
        22,
        "leader's transitive downstream closure size"
    );

    // --- orphans + violations: the committed fixture is clean on both (no
    // disconnected model, no backward layer edge). The cap/listing render is
    // exercised with hand-built StatsView literals in the overlay tests.
    assert_eq!(
        sv.orphan_models,
        Vec::<String>::new(),
        "fixture has no orphans"
    );
    assert_eq!(
        sv.layer_violations,
        Vec::<(String, String)>::new(),
        "fixture has no layer violations (matches layer_violation_edges)"
    );

    // Deterministic: a second compute is bit-identical.
    assert_eq!(a.compute_stats_view(), sv, "compute_stats_view is stable");
}

#[test]
fn sql_and_stats_scroll_through_detail_scroll() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    // SQL modal: DetailScroll steps sv.scroll; up saturates at 0.
    apply_action(&mut a, Action::SqlOpen);
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.scroll, 2),
        m => panic!("expected Sql, got {m:?}"),
    }
    for _ in 0..5 {
        apply_action(&mut a, Action::DetailScroll(Direction::Up));
    }
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.scroll, 0, "scroll up saturates at 0"),
        m => panic!("expected Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);

    // Stats modal: same plumbing.
    apply_action(&mut a, Action::StatsOpen);
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    match &a.mode {
        Mode::Stats(sv) => assert_eq!(sv.scroll, 1),
        m => panic!("expected Stats, got {m:?}"),
    }
}

#[test]
fn feature_toggle_stubs_are_safe_noops() {
    // The 5 view/pref toggles must never quit, emit effects, or leave
    // Selection mode — and the SELECTED MODEL (by unique_id, not raw index:
    // SortCycle legitimately reorders the list) must survive them all.
    let mut a = app();
    a.select_by_unique_id(FCT);
    for action in [
        Action::CycleLens,
        Action::BookmarkToggle,
        Action::BookmarkCycle,
        Action::SortCycle,
        Action::ToggleMinimap,
    ] {
        let out = apply_action(&mut a, action);
        assert!(!out.quit && out.effects.is_empty(), "{action:?} is a no-op");
        assert!(
            matches!(a.mode, Mode::Selection),
            "{action:?} keeps Selection"
        );
    }
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "no toggle moved the selection off the selected model"
    );
}

#[test]
fn path_edges_follow_cursor_and_edge_styles_dim_off_path() {
    // Cursor home: no path edges, and the styled layout leaves every connector
    // cell default (the frozen lens-off render). Cursor off-root: the path's
    // edges are canonical (parent, child) keys present in the layout's
    // edge_cells; their cells get the on_path band and every other edge dims.
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert!(
        a.lineage_path_edges().is_empty(),
        "cursor home: no path edges"
    );
    let lay = a.styled_lineage_layout().expect("non-empty lineage");
    let plain_cell = lay.edge_cells.values().flatten().next().copied();
    if let Some((x, y)) = plain_cell {
        assert_eq!(
            lay.grid.attr_at(x, y),
            crate::CellAttr::default(),
            "cursor home: connectors stay Plain"
        );
    }

    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let path_edges = a.lineage_path_edges();
    assert!(
        !path_edges.is_empty(),
        "off-root cursor produces path edges"
    );
    let lay = a.styled_lineage_layout().expect("non-empty lineage");
    for key in &path_edges {
        let cells = lay
            .edge_cells
            .get(key)
            .unwrap_or_else(|| panic!("path edge {key:?} exists in edge_cells"));
        for &(x, y) in cells {
            let attr = lay.grid.attr_at(x, y);
            assert!(attr.on_path, "path edge cell ({x},{y}) carries the band");
            assert!(!attr.dimmed, "path edge cell is not dimmed");
        }
    }
    // Some off-path edge exists in the FCT subgraph and dims. Skip cells shared
    // with a path edge (siblings into one child share its arrowhead cell, and
    // the path attr deliberately wins those).
    let path_set: std::collections::HashSet<_> = path_edges.iter().collect();
    let path_cells: std::collections::HashSet<(usize, usize)> = path_edges
        .iter()
        .filter_map(|k| lay.edge_cells.get(k))
        .flatten()
        .copied()
        .collect();
    let (x, y) = lay
        .edge_cells
        .iter()
        .filter(|(k, _)| !path_set.contains(k))
        .flat_map(|(_, cells)| cells)
        .find(|c| !path_cells.contains(c))
        .copied()
        .expect("an off-path connector cell exists");
    assert!(lay.grid.attr_at(x, y).dimmed, "off-path connector dims");
}

#[test]
fn toggle_density_reshapes_the_cached_layout_and_the_yank() {
    // `v` flips the density: the cached styled layout rebuilds 1-row boxes
    // (density is part of the cache key) and the ASCII yank IS the pane's
    // diagram, so it follows. Toggling back restores the 3-row geometry.
    let mut a = app();
    a.select_by_unique_id(FCT);
    let tall = a.styled_lineage_layout().expect("lineage");
    assert_eq!(tall.rects[FCT].height, 3, "default: comfortable boxes");
    let tall_yank = a.lineage_ascii().expect("yank");

    let out = apply_action(&mut a, Action::ToggleDensity);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.ui_state.density(), crate::Density::Compact);
    let flat = a.styled_lineage_layout().expect("lineage");
    assert_eq!(flat.rects[FCT].height, 1, "compact: 1-row boxes");
    assert!(
        flat.grid.height() < tall.grid.height(),
        "compact grid is shorter"
    );
    let flat_yank = a.lineage_ascii().expect("yank");
    assert_ne!(flat_yank, tall_yank, "the yank follows the density");
    assert_eq!(flat_yank, flat.grid.to_text(), "yank IS the pane's diagram");

    apply_action(&mut a, Action::ToggleDensity);
    assert_eq!(a.ui_state.density(), crate::Density::Comfortable);
    assert_eq!(
        a.styled_lineage_layout().expect("lineage").rects[FCT].height,
        3,
        "toggling back restores comfortable geometry"
    );
}

#[test]
fn compact_density_keeps_cursor_movement_and_path_highlight_working() {
    // The spatial cursor and the path/edge highlight read the CACHED layout's
    // geometry, which reshapes under compact density (1-row rects, attach row
    // = the node's own row): h must still step one column upstream, and the
    // path edges must still resolve to drawn connector cells carrying the band.
    let mut a = app();
    a.select_by_unique_id(FCT);
    apply_action(&mut a, Action::ToggleDensity);
    a.ui_state.set_focus(Focus::RightPane);

    let lay = a.styled_lineage_layout().expect("lineage");
    let root_col = lay.columns[FCT];
    assert!(root_col > 0, "fct has upstream columns");
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().expect("cursor");
    let lay = a.styled_lineage_layout().expect("lineage");
    assert_eq!(
        lay.columns[&cur],
        root_col - 1,
        "h steps one column upstream in compact too"
    );
    assert_eq!(lay.rects[&cur].height, 1, "compact rects stay 1-row");
    let path_edges = a.lineage_path_edges();
    assert!(!path_edges.is_empty(), "the path exists in compact mode");
    for key in &path_edges {
        let cells = &lay.edge_cells[key];
        assert!(!cells.is_empty(), "compact connectors recorded for {key:?}");
        for &(x, y) in cells {
            assert!(lay.grid.attr_at(x, y).on_path, "band on compact connector");
        }
    }
}

#[test]
fn layer_bands_label_unanimous_model_columns_only() {
    // Every band's tint must agree with the Layer lens's per-node tint for the
    // models in that column (the shared layer_tint mapping), bands are sorted
    // by x (BTreeMap by column), and a band never appears for a column whose
    // models disagree — or that holds no models at all.
    let mut a = app();
    a.select_by_unique_id(FCT);
    let lay = a.styled_lineage_layout().expect("lineage");
    let bands = a.layer_bands(&lay);
    assert!(
        !bands.is_empty(),
        "the fct subgraph has unanimous model columns"
    );
    let xs: Vec<usize> = bands.iter().map(|b| b.x).collect();
    let mut sorted = xs.clone();
    sorted.sort_unstable();
    assert_eq!(xs, sorted, "bands come in column order");
    for band in &bands {
        // Find the column's models and check unanimity against the band label.
        let col_uids: Vec<&String> = lay
            .rects
            .iter()
            .filter(|(_, r)| r.x == band.x)
            .map(|(uid, _)| uid)
            .collect();
        let model_layers: Vec<&str> = col_uids
            .iter()
            .filter_map(|uid| a.dag.get(uid))
            .filter(|n| n.resource_type == "model")
            .map(|n| crate::model_list::first_dir(n).unwrap_or("other"))
            .collect();
        assert!(
            !model_layers.is_empty(),
            "a band implies models in the column"
        );
        assert!(
            model_layers.iter().all(|l| *l == band.label),
            "band {band:?} must be unanimous over {model_layers:?}"
        );
    }
    // Determinism: two computations are identical.
    assert_eq!(bands, a.layer_bands(&lay));
}

#[test]
fn overlay_scroll_acts_only_inside_an_overlay() {
    let mut a = app();
    // In Selection mode, an overlay-scroll action is a no-op (no panic).
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    assert!(matches!(a.mode, Mode::Selection));
    // In Help mode it advances the help scroll.
    apply_action(&mut a, Action::HelpToggle);
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    match a.mode {
        Mode::Help { scroll } => assert_eq!(scroll, 2),
        _ => panic!("expected Help mode"),
    }
    // Up saturates at 0.
    for _ in 0..5 {
        apply_action(&mut a, Action::DetailScroll(Direction::Up));
    }
    match a.mode {
        Mode::Help { scroll } => assert_eq!(scroll, 0, "scroll up saturates at 0"),
        _ => panic!("expected Help mode"),
    }
}

// ---- Step B: test-coverage lens ('t') + automatic root↔cursor path ----

/// The one fixture node that is a coverage gap (testable, zero tests): the
/// delivery-lanes snapshot. All 45 models and all 7 seeds carry
/// tests, so the snapshot is the sole gap (verified against the fixture).
const GAP_SNAPSHOT: &str = "snapshot.jaffle_finance.delivery_lanes_snapshot";

#[test]
fn coverage_gap_predicate_over_resource_type_and_tests() {
    let a = app();
    // A tested model is NOT a gap; the snapshot with zero tests IS.
    let txn = a.dag.get("model.jaffle_finance.pos_txn").unwrap();
    assert!(
        txn.test_count > 0 && !coverage_gap(txn),
        "tested model: no gap"
    );
    let snap = a.dag.get(GAP_SNAPSHOT).unwrap();
    assert!(
        snap.test_count == 0 && coverage_gap(snap),
        "untested snapshot is a coverage gap"
    );
    // A source is NEVER a gap even with zero tests (excluded by type).
    let src = a
        .dag
        .nodes()
        .values()
        .find(|n| n.resource_type == "source" && n.test_count == 0)
        .expect("fixture has an untested source");
    assert!(!coverage_gap(src), "a source is never a coverage gap");
    // A synthetic untested model IS a gap (predicate is pure over NodeInfo).
    let m = NodeInfo {
        resource_type: "model".into(),
        test_count: 0,
        ..Default::default()
    };
    assert!(coverage_gap(&m), "an untested model is a gap");
}

#[test]
fn coverage_summary_counts_testable_resources() {
    // Fixture: 45 models + 7 seeds + 1 snapshot = 53 testable; only the
    // snapshot is untested → 52 tested.
    let a = app();
    assert_eq!(
        a.coverage_summary(),
        (52, 53),
        "coverage over model/snapshot/seed: 52 of 53 carry tests"
    );
}

#[test]
fn lineage_path_set_empty_when_cursor_home() {
    // Cursor home (== selection): the set is empty so a home cursor
    // highlights nothing.
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert_eq!(a.lineage_cursor_uid().as_deref(), Some(FCT), "cursor home");
    assert!(
        a.lineage_path_set().is_empty(),
        "a home cursor produces an empty path set"
    );
}

#[test]
fn lineage_path_set_connects_root_to_an_upstream_cursor() {
    // Off-root (upstream) cursor: the path contains both endpoints, every
    // member is adjacent to another in the (undirected) subgraph edges, and
    // the result is deterministic across two calls.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft); // cursor → one column upstream
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur, FCT, "cursor left the root");

    let path = a.lineage_path_set();
    assert!(path.contains(FCT), "path includes the root");
    assert!(path.contains(&cur), "path includes the cursor");
    assert!(path.len() >= 2);

    // Connectivity: every path node touches another path node via some edge
    // (treated undirected), so the highlighted set is one connected chain.
    let sg = a.lineage_subgraph();
    for uid in &path {
        let touches = sg.edges.iter().any(|e| {
            (e.parent == *uid && path.contains(&e.child))
                || (e.child == *uid && path.contains(&e.parent))
        });
        assert!(touches, "path node {uid} is connected to another path node");
    }
    // Deterministic.
    assert_eq!(path, a.lineage_path_set(), "two calls are identical");
}

#[test]
fn lineage_path_set_is_undirected_for_a_downstream_cursor() {
    // The cursor can sit DOWNSTREAM of the root (not just upstream): the
    // undirected BFS must still connect them. Root a node and place the
    // cursor on a known downstream model.
    let root = "model.jaffle_finance.int_delivery_lanes__combined";
    let down_model = "model.jaffle_finance.fct_delivery_monthly_snapshot";
    let mut a = app();
    a.select_by_unique_id(root);
    assert!(
        a.dag.downstream(root).contains(down_model),
        "precondition: the model is downstream of the root"
    );
    assert!(
        a.lineage_subgraph().contains(down_model),
        "the downstream model is in the rooted subgraph"
    );
    a.lineage_cursor = Some(down_model.to_string());
    let path = a.lineage_path_set();
    assert!(path.contains(root), "undirected path includes the root");
    assert!(
        path.contains(down_model),
        "undirected path includes the downstream cursor"
    );
    // Connected chain (undirected) and deterministic.
    let sg = a.lineage_subgraph();
    for uid in &path {
        let touches = sg.edges.iter().any(|e| {
            (e.parent == *uid && path.contains(&e.child))
                || (e.child == *uid && path.contains(&e.parent))
        });
        assert!(touches, "downstream path node {uid} stays connected");
    }
    assert_eq!(path, a.lineage_path_set(), "deterministic");
}

#[test]
fn coverage_lens_sets_warn_tint_only_when_active() {
    // Root on the gap snapshot so its own box is in the layout. With the lens
    // Off no attr carries a tint; cycling to Coverage marks the gap snapshot
    // Warn while a tested upstream model and a source stay None.
    let mut a = app();
    a.select_by_unique_id(FCT); // any selectable model; we layout the snapshot subgraph below
    let lay = crate::layout(&a.dag.subgraph(GAP_SNAPSHOT));

    let styles_off = a.lineage_styles(&lay);
    assert!(
        styles_off.values().all(|c| c.lens == LensTint::None),
        "lens off: nothing is tinted"
    );

    // One `t` press from default == Coverage (the old behaviour).
    a.ui_state.cycle_lens();
    assert_eq!(a.ui_state.lens(), LineageLens::Coverage);
    let styles_on = a.lineage_styles(&lay);
    assert_eq!(
        styles_on.get(GAP_SNAPSHOT).unwrap().lens,
        LensTint::Warn,
        "Coverage lens: the gap snapshot is Warn-tinted"
    );
    // A tested upstream model is NOT tinted.
    let tested_up = "model.jaffle_finance.stg_masterdata__companies";
    assert!(
        styles_on.contains_key(tested_up),
        "precondition: tested model is in the snapshot subgraph"
    );
    assert_eq!(
        styles_on.get(tested_up).unwrap().lens,
        LensTint::None,
        "a tested model is not tinted under the coverage lens"
    );
    // A source upstream is never tinted (excluded by coverage_gap).
    let src = lay
        .rects
        .keys()
        .find(|uid| uid.starts_with("source."))
        .expect("the snapshot subgraph has an upstream source");
    assert_eq!(
        styles_on.get(src).unwrap().lens,
        LensTint::None,
        "a source is never tinted under the coverage lens"
    );
}

/// Style one node's own box by rooting the lineage on it and reading its
/// `lens` tint — the node is always present in its own subgraph, so this is a
/// deterministic way to assert a lens's per-node tint on the real fixture.
fn tint_of(a: &mut App, lens: LineageLens, uid: &str) -> LensTint {
    a.select_by_unique_id(uid);
    while a.ui_state.lens() != lens {
        a.ui_state.cycle_lens();
    }
    let lay = crate::layout(&a.lineage_subgraph());
    a.lineage_styles(&lay).get(uid).copied().unwrap().lens
}

#[test]
fn degree_heat_lens_buckets_by_transitive_downstream() {
    // Fixture-anchored buckets (computed, not guessed): pos_files__assignment
    // has 16 transitive downstream (HeatHigh), stg_payment__shoppers has 1
    // (HeatLow), and the leaf int_shoppers__combined has 0 (None). The heat
    // metric is the raw Dag closure — deliberately UNSPLIT, so the fixture's 2
    // exposures count as consumers: FCT lands at 4 (HeatMid) and
    // stg_payment__suppliers at 7, crossing into HeatHigh.
    let mut a = app();
    let hub = "model.jaffle_finance.pos_files__assignment";
    let suppliers = "model.jaffle_finance.stg_payment__suppliers";
    let low = "model.jaffle_finance.stg_payment__shoppers";
    let leaf = "model.jaffle_finance.int_shoppers__combined";
    assert_eq!(a.dag.downstream(hub).len(), 16, "fixture hub blast radius");
    assert_eq!(
        a.dag.downstream(suppliers).len(),
        7,
        "suppliers blast radius (5 models + 2 exposures)"
    );
    assert_eq!(
        a.dag.downstream(FCT).len(),
        4,
        "fixture FCT blast radius (2 models + 2 exposures)"
    );
    assert_eq!(a.dag.downstream(low).len(), 1, "fixture low blast radius");
    assert_eq!(a.dag.downstream(leaf).len(), 0, "fixture leaf");
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, hub),
        LensTint::HeatHigh
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, suppliers),
        LensTint::HeatHigh,
        "the exposure consumers push suppliers across the high boundary"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, FCT),
        LensTint::HeatMid,
        "the exposure consumers push FCT into the mid bucket"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, low),
        LensTint::HeatLow
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, leaf),
        LensTint::None
    );
}

#[test]
fn layer_lens_tints_models_by_layer_and_leaves_non_models_untinted() {
    // Each layer maps to its own tint; a source keeps its class colour (None).
    let mut a = app();
    let staging = "model.jaffle_finance.stg_payment__shoppers";
    let inter = "model.jaffle_finance.int_shoppers__combined";
    let marts = FCT; // fct_subscription_process is a marts model
    assert_eq!(
        tint_of(&mut a, LineageLens::Layer, staging),
        LensTint::LayerStaging
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::Layer, inter),
        LensTint::LayerIntermediate
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::Layer, marts),
        LensTint::LayerMarts
    );
    // A source upstream of stg_payment__shoppers gets None (its class colour).
    a.select_by_unique_id(staging);
    while a.ui_state.lens() != LineageLens::Layer {
        a.ui_state.cycle_lens();
    }
    let lay = crate::layout(&a.lineage_subgraph());
    let styles = a.lineage_styles(&lay);
    let src = lay
        .rects
        .keys()
        .find(|uid| uid.starts_with("source."))
        .expect("staging subgraph has an upstream source");
    assert_eq!(
        styles.get(src).unwrap().lens,
        LensTint::None,
        "a source is untinted under the layer lens (keeps its class colour)"
    );
}

#[test]
fn layer_violation_edges_empty_on_a_clean_fixture_and_no_violation_tint() {
    // The committed fixture is a clean dbt project: no marts→staging-style
    // backward edge, so the violation set is empty and no node is tinted.
    let mut a = app();
    assert!(
        layer_violation_edges(&a.dag).is_empty(),
        "the clean fixture has no layer-violation edges"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::LayerViolation, FCT),
        LensTint::None,
        "no node is violation-tinted on a clean project"
    );
}

/// A tiny synthetic Dag with a deliberate layer violation: a `marts` model
/// (`mt`) feeds a `staging` model (`st`) — a backward edge — plus a clean
/// `staging → intermediate` edge (`st → it`) that is NOT a violation.
fn violation_dag() -> Dag {
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    let mut add = |id: &str, name: &str, path: &str| {
        nodes.insert(
            id.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(path.into()),
                ..Default::default()
            },
        );
    };
    add("model.p.mt", "mt", "marts/mt.sql");
    add("model.p.st", "st", "staging/st.sql");
    add("model.p.it", "it", "intermediate/it.sql");
    // child_map: mt → st (BACKWARD), st → it (clean). parent_map mirrors it.
    let mut child_map = HashMap::new();
    child_map.insert("model.p.mt".to_string(), vec!["model.p.st".to_string()]);
    child_map.insert("model.p.st".to_string(), vec!["model.p.it".to_string()]);
    let mut parent_map = HashMap::new();
    parent_map.insert("model.p.st".to_string(), vec!["model.p.mt".to_string()]);
    parent_map.insert("model.p.it".to_string(), vec!["model.p.st".to_string()]);
    Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map,
        child_map,
    })
}

#[test]
fn layer_violation_edges_finds_the_backward_edge_only() {
    // Exactly the marts→staging edge is a violation; the clean staging→inter
    // edge is not. Deterministic (sorted) output.
    let dag = violation_dag();
    assert_eq!(
        layer_violation_edges(&dag),
        vec![("model.p.mt".to_string(), "model.p.st".to_string())],
        "only the marts→staging backward edge is a violation"
    );
}

#[test]
fn violation_lens_tints_both_incident_nodes_and_spares_the_rest() {
    // Root on the marts model so the whole synthetic graph is laid out; under
    // the violation lens the two incident nodes (mt, st) are Violation-tinted,
    // the clean downstream `it` is not.
    let mut a = App::new(
        violation_dag(),
        PathBuf::from("/tmp/x/target/manifest.json"),
    );
    a.select_by_unique_id("model.p.mt");
    a.ui_state.cycle_lens(); // Coverage
    a.ui_state.cycle_lens(); // DegreeHeat
    a.ui_state.cycle_lens(); // Layer
    a.ui_state.cycle_lens(); // LayerViolation
    assert_eq!(a.ui_state.lens(), LineageLens::LayerViolation);
    let lay = crate::layout(&a.lineage_subgraph());
    let styles = a.lineage_styles(&lay);
    assert_eq!(
        styles.get("model.p.mt").unwrap().lens,
        LensTint::Violation,
        "the marts parent is violation-tinted"
    );
    assert_eq!(
        styles.get("model.p.st").unwrap().lens,
        LensTint::Violation,
        "the staging child is violation-tinted"
    );
    assert_eq!(
        styles.get("model.p.it").unwrap().lens,
        LensTint::None,
        "the clean downstream model is not violation-tinted"
    );
}

#[test]
fn lineage_styles_sets_on_path_for_an_off_root_cursor() {
    // With an off-root cursor, exactly the root↔cursor path nodes carry
    // on_path; nodes off the path do not.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur, FCT);

    let path = a.lineage_path_set();
    // Build the layout the loop draws (display subgraph), then style it.
    let lay = crate::layout(&a.lineage_display_subgraph());
    let styles = a.lineage_styles(&lay);
    for (uid, attr) in &styles {
        assert_eq!(
            attr.on_path,
            path.contains(uid),
            "on_path matches the path set for {uid}"
        );
    }
    // The two endpoints are on the path.
    assert!(styles.get(FCT).unwrap().on_path, "root is on the path");
    assert!(styles.get(&cur).unwrap().on_path, "cursor is on the path");

    // Cursor home → no on_path anywhere.
    a.reset_lineage_cursor();
    let styles_home = a.lineage_styles(&lay);
    assert!(
        styles_home.values().all(|c| !c.on_path),
        "a home cursor highlights no path"
    );
}

#[test]
fn focus_dim_is_set_only_off_root_and_only_for_off_path_nodes() {
    // Focus dim is lens-INDEPENDENT (no lens active here): with the cursor at
    // home NOTHING is dimmed; with an off-root cursor, every node NOT on the
    // root↔cursor path is dimmed and the path nodes are not.
    let mut a = app();
    a.select_by_unique_id(FCT);

    // Home cursor → empty path → no dim anywhere (the diagram reads normally).
    let lay_home = crate::layout(&a.lineage_display_subgraph());
    let styles_home = a.lineage_styles(&lay_home);
    assert!(
        styles_home.values().all(|c| !c.dimmed),
        "a home cursor dims nothing"
    );

    // Off-root cursor → dim partitions exactly on the path set.
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur, FCT, "precondition: cursor walked off the root");
    let path = a.lineage_path_set();
    assert!(!path.is_empty(), "off-root path is non-empty");
    let lay = crate::layout(&a.lineage_display_subgraph());
    let styles = a.lineage_styles(&lay);
    for (uid, attr) in &styles {
        assert_eq!(
            attr.dimmed,
            !path.contains(uid),
            "off-path nodes are dimmed, path nodes are not ({uid})"
        );
    }
    // The two endpoints are on the path, so never dimmed.
    assert!(!styles.get(FCT).unwrap().dimmed, "root never dims");
    assert!(!styles.get(&cur).unwrap().dimmed, "cursor never dims");
    // At least one off-path node exists in fct's subgraph and IS dimmed.
    assert!(
        styles.values().any(|c| c.dimmed),
        "fct's subgraph has off-path nodes that dim"
    );
}

#[test]
fn cycle_lens_advances_the_lens_as_a_noop_action() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.ui_state.selected();
    assert_eq!(a.ui_state.lens(), LineageLens::Off, "lens starts Off");
    let out = apply_action(&mut a, Action::CycleLens);
    assert!(
        !out.quit && out.effects.is_empty(),
        "cycle is a no-op action"
    );
    assert!(matches!(a.mode, Mode::Selection), "stays in Selection");
    assert_eq!(
        a.ui_state.lens(),
        LineageLens::Coverage,
        "advanced to Coverage"
    );
    // Cycle through the rest and back to Off.
    for expected in [
        LineageLens::DegreeHeat,
        LineageLens::Layer,
        LineageLens::LayerViolation,
        LineageLens::Off,
    ] {
        apply_action(&mut a, Action::CycleLens);
        assert_eq!(a.ui_state.lens(), expected);
    }
    assert_eq!(a.ui_state.selected(), before, "selection never moved");
}

#[test]
fn toggle_minimap_flips_the_flag_as_a_noop_action() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.ui_state.selected();
    assert!(
        !a.ui_state.minimap_visible(),
        "minimap starts off (default)"
    );
    let out = apply_action(&mut a, Action::ToggleMinimap);
    assert!(
        !out.quit && out.effects.is_empty(),
        "toggle is a no-op action"
    );
    assert!(matches!(a.mode, Mode::Selection), "stays in Selection");
    assert!(a.ui_state.minimap_visible(), "the flag flipped on");
    apply_action(&mut a, Action::ToggleMinimap);
    assert!(!a.ui_state.minimap_visible(), "and flips back off");
    assert_eq!(a.ui_state.selected(), before, "selection never moved");
}

#[test]
fn bookmark_toggle_inserts_then_removes_on_selected_uid() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert!(a.bookmarks.is_empty(), "no bookmarks initially");
    apply_action(&mut a, Action::BookmarkToggle);
    assert!(a.bookmarks.contains(FCT), "first toggle adds the bookmark");
    apply_action(&mut a, Action::BookmarkToggle);
    assert!(!a.bookmarks.contains(FCT), "second toggle removes it");
    assert!(a.bookmarks.is_empty());
}

#[test]
fn bookmark_toggle_is_a_noop_when_nothing_selected() {
    // An empty filter leaves no selectable model, so selected_unique_id is
    // None and the toggle does nothing (no panic, no insert).
    let mut a = app();
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: "zzzzzz".into(), // matches no model
        origin_uid: None,
        match_idx: 0,
    });
    a.refilter();
    assert!(a.selected_node().is_none(), "filter matched nothing");
    apply_action(&mut a, Action::BookmarkToggle);
    assert!(
        a.bookmarks.is_empty(),
        "toggle with no selection is a no-op"
    );
}

#[test]
fn bookmark_cycle_wraps_to_next_bookmarked_model() {
    let mut a = app();
    // Bookmark two models; cycling from one lands on the other, and again
    // wraps back. Use the flat models list to pick deterministic neighbours.
    let ids: Vec<String> = a
        .model_list
        .models
        .iter()
        .map(|m| m.unique_id.clone())
        .collect();
    let first = ids[1].clone(); // not index 0, so the wrap is exercised
    let second = ids[5].clone();
    a.bookmarks.insert(first.clone());
    a.bookmarks.insert(second.clone());
    a.select_by_unique_id(&first);
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(second.as_str()),
        "cycle moves to the next bookmarked model"
    );
    // From the later one, cycling wraps forward back to the earlier one.
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(first.as_str()),
        "cycle wraps around to the first bookmark"
    );
}

#[test]
fn bookmark_cycle_is_a_noop_when_set_empty() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.selected_unique_id();
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(a.selected_unique_id(), before, "no bookmarks ⇒ no move");
}

#[test]
fn reload_prunes_vanished_bookmarks_and_keeps_survivors() {
    let mut a = app();
    // One real id (survives reload) and one stale id (pruned).
    a.bookmarks.insert(FCT.to_string());
    a.bookmarks
        .insert("model.jaffle_finance.gone_forever".to_string());
    a.reload().expect("reload ok");
    assert!(a.bookmarks.contains(FCT), "surviving id kept");
    assert!(
        !a.bookmarks.contains("model.jaffle_finance.gone_forever"),
        "vanished id pruned"
    );
    assert_eq!(a.bookmarks.len(), 1);
}

#[test]
fn sort_cycle_advances_mode_and_preserves_selection_by_uid() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert_eq!(a.sort, SortMode::Layer, "starts in Layer");
    apply_action(&mut a, Action::SortCycle);
    assert_eq!(a.sort, SortMode::Downstream, "cycles Layer→Downstream");
    // Selection is re-resolved BY uid across the rebuild — same node, even if
    // its row moved within its layer group.
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "selection survives the sort rebuild by unique_id"
    );
    apply_action(&mut a, Action::SortCycle);
    assert_eq!(a.sort, SortMode::Tests);
    apply_action(&mut a, Action::SortCycle);
    assert_eq!(a.sort, SortMode::Layer, "wraps back to Layer");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "still the same node after a full cycle"
    );
}

#[test]
fn sort_cycle_refilters_in_the_new_order_during_search() {
    // With a list search active, SortCycle rebuilds the full list AND the
    // filtered view, keeping the selection by uid. The active list reorders.
    let mut a = app();
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: "pos".into(),
        origin_uid: None,
        match_idx: 0,
    });
    a.refilter();
    let before: Vec<String> = a
        .active_list()
        .models
        .iter()
        .map(|m| m.unique_id.clone())
        .collect();
    assert!(before.len() > 1, "search matched several models");
    let sel = a.selected_unique_id();
    apply_action(&mut a, Action::SortCycle);
    // Still filtered (search mode preserved) and selection survives by uid.
    assert!(a.filter.is_some(), "filter rebuilt, search still active");
    assert_eq!(a.selected_unique_id(), sel, "selection unchanged by uid");
    let after: Vec<String> = a
        .active_list()
        .models
        .iter()
        .map(|m| m.unique_id.clone())
        .collect();
    // Same membership (a multiset), regardless of order.
    let mut b = before.clone();
    let mut af = after.clone();
    b.sort();
    af.sort();
    assert_eq!(b, af, "filtered membership is unchanged by the sort");
}

// ---- command palette round-trips ----

/// Open the palette, type `query`, then return the resulting `App` so a test
/// can confirm the filter/selection state before Enter.
fn open_palette_and_type(query: &str) -> App {
    let mut a = app();
    apply_action(&mut a, Action::PaletteOpen);
    assert!(matches!(a.mode, Mode::Palette(_)), "PaletteOpen → Palette");
    for c in query.chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    a
}

#[test]
fn palette_open_type_enter_runs_the_resolved_action() {
    // Filter to the minimap toggle and run it: the minimap pref flips and the
    // mode returns to Selection (the recursively-applied action's effect lands).
    let mut a = open_palette_and_type("minimap");
    let before = a.ui_state.minimap_visible();
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.mode, Mode::Selection, "palette closes on confirm");
    assert_ne!(
        a.ui_state.minimap_visible(),
        before,
        "the resolved ToggleMinimap action ran"
    );
}

#[test]
fn palette_confirm_propagates_quit_outcome() {
    // Choosing the quit command must quit (the whole Outcome propagates).
    let mut a = open_palette_and_type("quit");
    // "quit" is the only command whose help contains that subsequence.
    assert!(
        palette_candidates("quit")
            .iter()
            .any(|b| b.action == Action::Quit),
        "the quit command is a candidate"
    );
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(out.quit, "confirming the quit command quits");
    assert_eq!(
        a.mode,
        Mode::Selection,
        "mode set to Selection before apply"
    );
}

#[test]
fn palette_confirm_propagates_editor_effect() {
    // Choosing "open SQL in $EDITOR" must yield the editor Effect. A model
    // with a resolvable file path is selected first so the effect is produced.
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::PaletteOpen);
    for c in "$EDITOR".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    // The OpenEditor row's help is "open SQL in $EDITOR".
    assert!(
        palette_candidates("$EDITOR")
            .iter()
            .any(|b| b.action == Action::OpenEditor),
        "the editor command is a candidate"
    );
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(
        matches!(out.effects.as_slice(), [Effect::OpenEditor(_)]),
        "confirming the editor command emits its Effect: {:?}",
        out.effects
    );
    assert_eq!(a.mode, Mode::Selection);
}

#[test]
fn palette_selected_clamps_when_the_filter_shrinks() {
    // Move the cursor down on the full list, then narrow the query so fewer
    // candidates remain: `selected` must clamp into range (never out of bounds).
    let mut a = open_palette_and_type("");
    // Step down a few times over the full candidate list.
    for _ in 0..5 {
        apply_action(&mut a, Action::SearchMove(Direction::Down));
    }
    let moved = match &a.mode {
        Mode::Palette(p) => p.selected,
        _ => unreachable!(),
    };
    assert!(moved > 0, "the palette cursor moved down");
    // Now type a narrowing query; `selected` resets to 0 on each keystroke and
    // can never exceed the (smaller) candidate count.
    for c in "lens".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    if let Mode::Palette(p) = &a.mode {
        let count = palette_candidates(&p.query).len();
        assert!(count > 0, "'lens' still has candidates");
        assert!(p.selected < count, "selected stays in range after shrink");
    } else {
        unreachable!();
    }
}

#[test]
fn palette_cancel_returns_to_selection_without_touching_the_filter() {
    // Esc closes the palette and leaves the list filter untouched (the palette
    // shares the SearchCancel action but never drives the list filter).
    let mut a = open_palette_and_type("lens");
    assert!(a.filter.is_none(), "the palette never builds a list filter");
    let out = apply_action(&mut a, Action::SearchCancel);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.mode, Mode::Selection, "Esc closes the palette");
    assert!(a.filter.is_none(), "filter still untouched after cancel");
}

#[test]
fn palette_backspace_resets_selected_to_top() {
    // Backspace resets `selected` to 0 EXACTLY like typing, so the highlighted
    // row is a pure function of the query (no arrival-path dependence). Force a
    // non-zero selection, backspace, and assert it snapped back to the top.
    let mut a = open_palette_and_type("lens");
    if let Mode::Palette(p) = &mut a.mode {
        p.selected = palette_candidates(&p.query).len() - 1;
        assert!(
            p.selected > 0,
            "fixture has >1 'lens' candidate to move off 0"
        );
    }
    apply_action(&mut a, Action::SearchBackspace); // "len"
    if let Mode::Palette(p) = &a.mode {
        assert_eq!(p.selected, 0, "backspace resets the highlight to the top");
    } else {
        unreachable!();
    }
}

// ---- --diff baseline + Diff lens ------------------------------------------

/// Two tiny synthetic Dags for diffing: the BASE has models a -> b; the
/// CURRENT keeps a (unchanged), changes b's materialization (modified), and
/// adds c downstream of b (added). The base also has a model `gone` the
/// current drops (removed).
fn diff_base_dag() -> Dag {
    use crate::{RawConfig, RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    let mut add = |id: &str, name: &str, mat: &str| {
        nodes.insert(
            id.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(format!("marts/{name}.sql")),
                config: RawConfig {
                    materialized: Some(mat.into()),
                },
                ..Default::default()
            },
        );
    };
    add("model.p.a", "a", "view");
    add("model.p.b", "b", "table");
    add("model.p.gone", "gone", "view");
    Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map: HashMap::from([("model.p.b".to_string(), vec!["model.p.a".to_string()])]),
        child_map: HashMap::from([("model.p.a".to_string(), vec!["model.p.b".to_string()])]),
    })
}

fn diff_current_dag() -> Dag {
    use crate::{RawConfig, RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    let mut add = |id: &str, name: &str, mat: &str| {
        nodes.insert(
            id.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(format!("marts/{name}.sql")),
                config: RawConfig {
                    materialized: Some(mat.into()),
                },
                ..Default::default()
            },
        );
    };
    add("model.p.a", "a", "view");
    add("model.p.b", "b", "incremental"); // modified vs the base
    add("model.p.c", "c", "view"); // added vs the base
    Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map: HashMap::from([
            ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
            ("model.p.c".to_string(), vec!["model.p.b".to_string()]),
        ]),
        child_map: HashMap::from([
            ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
            ("model.p.b".to_string(), vec!["model.p.c".to_string()]),
        ]),
    })
}

/// An App over the synthetic CURRENT dag with the BASE loaded as its --diff
/// baseline (`+1 ~1 -1` plus one added edge).
fn diff_app() -> App {
    let mut a = App::new(
        diff_current_dag(),
        PathBuf::from("/tmp/x/target/manifest.json"),
    );
    a.set_diff_base(diff_base_dag(), "base/manifest.json".to_string());
    a
}

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
    // Without a baseline the apply_action cycle is the legacy 5-step ring.
    let mut a = app();
    for expected in [
        LineageLens::Coverage,
        LineageLens::DegreeHeat,
        LineageLens::Layer,
        LineageLens::LayerViolation,
        LineageLens::Off,
    ] {
        apply_action(&mut a, Action::CycleLens);
        assert_eq!(a.ui_state.lens(), expected, "Diff is skipped");
    }
    // With a baseline the Diff slot joins the ring after LayerViolation.
    let mut a = diff_app();
    a.ui_state.set_lens(LineageLens::Off);
    for expected in [
        LineageLens::Coverage,
        LineageLens::DegreeHeat,
        LineageLens::Layer,
        LineageLens::LayerViolation,
        LineageLens::Diff,
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

/// A model node for a hand-built PR-impact dag, in a given layer dir and
/// SQL-stamped so a `raw_code` change reads as "modified".
fn pr_model(name: &str, layer: &str, sql: &str) -> crate::RawNode {
    crate::RawNode {
        name: name.into(),
        resource_type: "model".into(),
        path: Some(format!("{layer}/{name}.sql")),
        raw_code: Some(sql.to_string()),
        config: crate::RawConfig {
            materialized: Some("view".into()),
        },
        ..Default::default()
    }
}

/// Assemble a `Dag` from `(uid, node)` models, `(parent, child)` edges, and
/// `(uid, exposure)` consumers — the building block for the PR-impact scenarios.
fn pr_dag(
    nodes: Vec<(&str, crate::RawNode)>,
    edges: &[(&str, &str)],
    exposures: Vec<(&str, crate::RawExposure)>,
) -> Dag {
    use std::collections::HashMap;
    let mut node_map = HashMap::new();
    for (uid, n) in nodes {
        node_map.insert(uid.to_string(), n);
    }
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    for (p, c) in edges {
        parent_map
            .entry(c.to_string())
            .or_default()
            .push(p.to_string());
        child_map
            .entry(p.to_string())
            .or_default()
            .push(c.to_string());
    }
    let mut exposure_map = HashMap::new();
    for (uid, e) in exposures {
        exposure_map.insert(uid.to_string(), e);
    }
    Dag::build(&crate::RawManifest {
        nodes: node_map,
        sources: HashMap::new(),
        exposures: exposure_map,
        parent_map,
        child_map,
    })
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
    use super::analysis::PR_HUB_MIN_DOWNSTREAM as MIN;
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

// ---- whole-graph overview (`w`) ----

#[test]
fn overview_toggle_shows_whole_graph_and_invalidates_caches() {
    let mut a = app();
    let rooted = a.subgraph_rc().nodes.len();
    assert!(
        rooted < a.dag.len(),
        "precondition: the default selection roots a partial subgraph"
    );

    let out = apply_action(&mut a, Action::ToggleOverview);
    assert!(!out.quit && out.effects.is_empty(), "a pure state toggle");
    assert_eq!(
        a.subgraph_rc().nodes.len(),
        a.dag.len(),
        "overview shows every node"
    );
    assert_eq!(
        a.subgraph_rc().selected,
        a.selected_unique_id().unwrap(),
        "the current selection is still the emphasised node"
    );
    assert_eq!(
        a.styled_lineage_layout()
            .expect("overview has a layout")
            .rects
            .len(),
        a.dag.len(),
        "every node gets a rect"
    );
    let note = a.take_notice().expect("entering the overview toasts");
    assert!(note.starts_with("overview: "), "got {note:?}");
    assert!(
        a.ui_state.minimap_visible(),
        "entering the overview turns the minimap on"
    );

    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(
        a.subgraph_rc().nodes.len(),
        rooted,
        "toggling back restores the rooted subgraph"
    );
}

#[test]
fn overview_gates_view_and_density_toggles() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    a.take_notice(); // drain the entry toast

    let view_before = a.lineage_view.clone();
    let density_before = a.ui_state.density();
    for action in [
        Action::ToggleUpstream,
        Action::DepthDecrease,
        Action::ToggleDensity,
    ] {
        apply_action(&mut a, action);
        assert_eq!(
            a.lineage_view, view_before,
            "the view filters never change in overview"
        );
        assert_eq!(
            a.ui_state.density(),
            density_before,
            "the density pref never changes in overview"
        );
        assert_eq!(
            a.take_notice().as_deref(),
            Some("view filters are off in overview (w to exit)")
        );
    }
}

#[test]
fn jump_to_drills_out_of_overview() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    assert!(a.ui_state.overview(), "overview is on");

    // The smallest model uid in the fixture, deterministically distinct from
    // the default selection.
    let target = "model.jaffle_finance.dim_delivery_lanes";
    assert_ne!(a.selected_unique_id().as_deref(), Some(target));
    a.jump_to(target);
    assert!(
        !a.ui_state.overview(),
        "a successful re-root drills out of the overview"
    );
    assert_eq!(a.selected_unique_id().as_deref(), Some(target));
}

#[test]
fn overview_forces_compact_and_ascii_yank_matches() {
    let mut a = app();
    assert_eq!(
        a.ui_state.density(),
        crate::Density::Comfortable,
        "precondition: default density"
    );
    let rooted_lines = a.lineage_ascii().expect("rooted yank").lines().count();

    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(
        a.ui_state.density(),
        crate::Density::Comfortable,
        "the user's density pref is untouched"
    );
    assert_eq!(
        a.effective_density(),
        crate::Density::Compact,
        "the overview forces compact rendering"
    );
    let overview_lines = a.lineage_ascii().expect("overview yank").lines().count();
    assert!(
        overview_lines > rooted_lines,
        "the whole-graph yank has more lines than the rooted one"
    );
}

#[test]
fn overview_suppresses_breadcrumb_and_path() {
    let mut a = app();
    a.jump_to(FCT); // builds back-history
    assert!(
        a.breadcrumb(usize::MAX).is_some(),
        "precondition: a breadcrumb exists"
    );

    apply_action(&mut a, Action::ToggleOverview);
    assert!(
        a.breadcrumb(usize::MAX).is_none(),
        "the breadcrumb is suppressed while the overview is on"
    );

    a.move_lineage_cursor(Direction::Right);
    assert!(
        a.lineage_path_set().is_empty(),
        "no root-cursor path while the overview is on"
    );
}

#[test]
fn overview_export_names_after_project() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    let out = apply_action(&mut a, Action::ExportLineage);
    let [Effect::WriteFile { path, .. }] = out.effects.as_slice() else {
        panic!("ExportLineage writes a file: {:?}", out.effects);
    };
    assert!(
        path.ends_with("_overview.txt"),
        "the overview export is named after the project, got {path:?}"
    );
}

#[test]
fn overview_search_matches_the_whole_graph() {
    let mut a = app();
    let root_sg = a.lineage_subgraph();
    // `lineage_matches` is a fuzzy subsequence match, so pick the smallest
    // (deterministic) outside-source name that ALSO fails to fuzzy-match
    // anything in the (tiny) rooted subgraph — a plain membership check isn't
    // enough to guarantee the rooted-view precondition below.
    let mut outside_names: Vec<String> = a
        .dag
        .nodes()
        .values()
        .filter(|n| n.resource_type == "source" && !root_sg.contains(&n.unique_id))
        .map(|n| n.name.clone())
        .collect();
    outside_names.sort();
    let outside_name = outside_names
        .into_iter()
        .find(|name| a.lineage_matches(name).is_empty())
        .expect("fixture has a source name the rooted view can't fuzzy-match");

    apply_action(&mut a, Action::ToggleOverview);
    assert!(
        !a.lineage_matches(&outside_name).is_empty(),
        "the overview reaches the whole graph"
    );
}
