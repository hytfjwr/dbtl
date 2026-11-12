//! `warehouse` — catalog-backed rules (`Requires::Catalog`): the declared
//! schema vs. what `dbt docs generate`'s catalog.json actually reports.
//! Both rules judge models/seeds/snapshots and skip nodes with no catalog
//! record at all — an absent catalog entry is "unknown", never "wrong".

use super::*;
use crate::{Catalog, SqlDialect};

/// `warehouse_type_drift`: a documented column's declared `data_type`
/// differs from the catalog's recorded type — the SAME comparison the
/// TUI's `drift:`-adjacent type lines run ([`CatalogNode::type_drift`]
/// with [`crate::types_equivalent`]: names lowercase, args compare only
/// when both sides carry them, alias tables by `dialect` so a BigQuery
/// `INT64` vs a declared `bigint` isn't drift). A column with no declared
/// type can't disagree; a node with no catalog record isn't judged.
pub(super) fn warehouse_type_drift(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    catalog: &Catalog,
    dialect: SqlDialect,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !matches!(n.resource_type.as_str(), "model" | "seed" | "snapshot") || !in_scope(n) {
            continue;
        }
        let Some(node) = catalog.get(uid) else {
            continue;
        };
        let Some(detail) = dag.detail(uid) else {
            continue;
        };
        let drifted: Vec<String> = node
            .type_drift(&detail.columns, dialect)
            .into_iter()
            .filter_map(|d| {
                d.declared.map(|decl| {
                    format!(
                        "{} (declared: {}, warehouse: {})",
                        d.name, decl, d.warehouse
                    )
                })
            })
            .collect();
        if !drifted.is_empty() {
            out.push(Finding::new(
                LintRule::WarehouseTypeDrift,
                (*uid).clone(),
                vec![],
                format!(
                    "{} declares {} disagreeing with the warehouse: {}",
                    n.name,
                    pluralize(drifted.len(), "column type"),
                    drifted.join(", ")
                ),
            ));
        }
    }
}

/// `columns_missing_in_warehouse`: a documented column the catalog doesn't
/// know — [`CatalogNode::drift`]'s "declared but not in warehouse" side.
/// Undocumented extras the warehouse carries (`missing_in_docs`) are a
/// docs coverage question, not this rule.
pub(super) fn columns_missing_in_warehouse(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    catalog: &Catalog,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !matches!(n.resource_type.as_str(), "model" | "seed" | "snapshot") || !in_scope(n) {
            continue;
        }
        let Some(node) = catalog.get(uid) else {
            continue;
        };
        let Some(detail) = dag.detail(uid) else {
            continue;
        };
        let documented: Vec<String> = detail.columns.iter().map(|c| c.name.clone()).collect();
        let (missing, _) = node.drift(&documented);
        if !missing.is_empty() {
            out.push(Finding::new(
                LintRule::ColumnsMissingInWarehouse,
                (*uid).clone(),
                vec![],
                format!(
                    "{} declares {} missing in the warehouse: {}",
                    n.name,
                    pluralize(missing.len(), "column"),
                    missing.join(", ")
                ),
            ));
        }
    }
}
