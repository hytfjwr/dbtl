use super::*;

// ---- lineage breadcrumb ----

#[test]
fn breadcrumb_is_none_with_empty_history() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    assert!(a.tab().back.is_empty());
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
    a.tab_mut().back = vec![
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
    a.tab_mut().back = vec!["model.jaffle_finance.stg_payment__shoppers".into()];
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
    a.tab_mut().back = vec![
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
