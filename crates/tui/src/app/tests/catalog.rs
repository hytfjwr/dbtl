use super::*;
use crate::ColumnResolver;

/// Write a catalog.json into a unique temp path — the same file `--catalog`
/// points at. `nodes` maps `unique_id` to its declared columns
/// (`{col_name: {type: "..."}}` entries are optional per column).
fn write_catalog(nodes: &[(&str, &[&str])]) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "dbtl_cat_test_{}_{:?}.json",
        std::process::id(),
        std::thread::current().id()
    ));
    let nodes: serde_json::Map<String, serde_json::Value> = nodes
        .iter()
        .map(|(uid, cols)| {
            (
                uid.to_string(),
                serde_json::json!({
                    "columns": cols.iter()
                        .map(|c| (c.to_string(), serde_json::json!({"type": "INT64"})))
                        .collect::<serde_json::Map<String, serde_json::Value>>(),
                }),
            )
        })
        .collect();
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({"nodes": nodes})).unwrap(),
    )
    .expect("write temp catalog");
    path
}

#[test]
fn set_catalog_loads_and_overrides_the_cll_column_list() {
    let mut a = app();
    let gen_before = a.generation;
    // The catalog's column list tops the resolver's priority chain: a
    // warehouse-only column answers where the manifest documented none.
    let path = write_catalog(&[(FCT, &["zzz_warehouse_col"])]);
    a.set_catalog(path.clone()).expect("explicit catalog loads");
    assert!(a.catalog().is_some(), "the loaded catalog is stored");
    assert!(a.generation > gen_before, "the side-map input restamps");
    let known = a.cll.columns(&a.dag, FCT);
    assert_eq!(
        known,
        vec!["zzz_warehouse_col".to_string()],
        "the catalog REPLACES the documented column list (it is the top tier)"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn set_catalog_survives_reload() {
    let mut a = app();
    let path = write_catalog(&[(FCT, &["zzz_warehouse_col"])]);
    a.set_catalog(path.clone()).expect("explicit catalog loads");
    let gen_after_set = a.generation;
    // A reload re-reads the recorded path — the explicit override is not
    // dropped back to auto-detect.
    a.reload().expect("reload");
    assert!(
        a.catalog().is_some(),
        "the explicit catalog is reloaded, not lost"
    );
    let known = a.cll.columns(&a.dag, FCT);
    assert_eq!(known, vec!["zzz_warehouse_col".to_string()]);
    assert!(a.generation > gen_after_set, "reload restamps too");
    let _ = std::fs::remove_file(path);
}

#[test]
fn set_catalog_error_leaves_the_auto_detected_state_untouched() {
    let mut a = app();
    assert!(
        a.catalog().is_none(),
        "no catalog.json sits next to the fixture"
    );
    let err = a
        .set_catalog(PathBuf::from("/nonexistent/catalog.json"))
        .expect_err("a bad explicit path is a hard error");
    assert!(err.to_string().contains("failed to load catalog"));
    assert!(a.catalog().is_none(), "nothing was partially stored");
}
