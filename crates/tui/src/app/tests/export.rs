use super::*;

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
    use dbtl_core::mermaid::mermaid_ids;
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
    use dbtl_core::mermaid::mermaid_label;
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
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes: HashMap::from([(
            "model.p.evil".to_string(),
            RawNode {
                name: "evil".into(),
                resource_type: "model".into(),
                config: RawConfig {
                    materialized: Some("table\"] x[\"pwn".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        docs: Default::default(),
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
    // The yank IS the pane's diagram: same (collapsed) display subgraph, same
    // glyphs — under this app's pinned `0` threshold the two are identical.
    let expected = crate::layout_mode(&a.lineage_display_subgraph(), a.glyph_mode)
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
    node_click(&mut a, exp);
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
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
        macros: HashMap::new(),
        docs: Default::default(),
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
