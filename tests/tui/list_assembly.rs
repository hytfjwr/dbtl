use super::*;

// ============================================================================
// list-assembly logic (deterministic, TTY-free core gate)
// ============================================================================

#[test]
pub(super) fn groups_are_four_in_fixed_logical_order() {
    let list = fixture_list();
    let layers: Vec<&str> = list.groups.iter().map(|g| g.layer.as_str()).collect();
    assert_eq!(
        layers,
        vec!["staging", "intermediate", "marts", "utilities"],
        "must be the 4 dbt layers in logical (not alphabetical) order"
    );
}

#[test]
pub(super) fn group_counts_are_frozen() {
    let list = fixture_list();
    let count = |layer: &str| {
        list.groups
            .iter()
            .find(|g| g.layer == layer)
            .map(|g| g.models.len())
            .unwrap_or(0)
    };
    assert_eq!(count("staging"), 11, "staging count");
    assert_eq!(count("intermediate"), 8, "intermediate count");
    assert_eq!(count("marts"), 9, "marts count");
    assert_eq!(count("utilities"), 17, "utilities count");
    assert_eq!(list.len(), 45, "total selectable models");
}

#[test]
pub(super) fn each_group_is_name_ascending() {
    let list = fixture_list();
    for group in &list.groups {
        let names: Vec<&str> = group.models.iter().map(|m| m.name.as_str()).collect();
        for pair in names.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "group {} not name-ascending: {:?} > {:?}",
                group.layer,
                pair[0],
                pair[1]
            );
        }
    }
}

#[test]
pub(super) fn group_first_and_last_names_are_frozen() {
    let list = fixture_list();
    let ends = |layer: &str| {
        let g = list.groups.iter().find(|g| g.layer == layer).unwrap();
        (
            g.models.first().unwrap().name.clone(),
            g.models.last().unwrap().name.clone(),
        )
    };
    assert_eq!(
        ends("staging"),
        (
            "stg_finance__budgets".into(),
            "stg_payment__warehouses".into()
        )
    );
    assert_eq!(
        ends("intermediate"),
        (
            "int_delivery_lanes__combined".into(),
            "int_suppliers__combined".into()
        )
    );
    assert_eq!(
        ends("marts"),
        (
            "dim_delivery_lanes".into(),
            "rpt_delivery_shopper_monthly_visits".into()
        )
    );
    assert_eq!(ends("utilities"), ("pos_cat".into(), "pos_txn".into()));
}

#[test]
pub(super) fn flat_display_order_index_0_and_44_are_frozen() {
    let list = fixture_list();
    assert_eq!(
        list.models[0].unique_id, "model.jaffle_finance.stg_finance__budgets",
        "index 0 = initial selection"
    );
    assert_eq!(
        list.models[44].unique_id, "model.jaffle_finance.pos_txn",
        "index 44 = G jump / bottom clamp target"
    );
}
