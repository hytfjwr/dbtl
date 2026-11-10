use super::*;

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
