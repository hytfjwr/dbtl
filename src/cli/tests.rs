use super::*;
use dbtl::{Dag, RawManifest, RawMetadata, RawNode};
use std::collections::HashMap;

/// A `Dag` whose recorded root project is `project_name`, built from
/// model `unique_id`s (each `model.<pkg>.<name>` — the package segment is
/// what `--project-only` scopes by).
fn dag_with(project_name: Option<&str>, uids: &[&str]) -> Dag {
    let nodes = uids
        .iter()
        .map(|uid| {
            (
                uid.to_string(),
                RawNode {
                    name: uid.rsplit('.').next().unwrap_or(uid).into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            )
        })
        .collect();
    Dag::build(&RawManifest {
        metadata: RawMetadata {
            project_name: project_name.map(str::to_string),
            ..Default::default()
        },
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
    })
}

#[test]
fn root_scope_prunes_foreign_package_nodes() {
    // A deps-only project: the recorded root `zzz_proj` contributes NO
    // nodes — every kept node belongs to an installed package. The strict
    // `Root` scope keeps none of them (it warns on stderr instead of
    // re-scoping to a foreign package, which would show the exact opposite
    // of the scope's intent).
    let dag = dag_with(Some("zzz_proj"), &["model.aaa_pkg.a", "model.aaa_pkg.b"]);
    let (scoped, _) = apply_package_scope(dag, dbtl::PackageScope::Root, false);
    assert!(
        scoped.is_empty(),
        "the scope stays strict — no foreign nodes"
    );
}

#[test]
fn root_scope_keeps_root_nodes() {
    // The normal case: the root project's nodes survive, foreign ones are
    // dropped, no warning.
    let dag = dag_with(
        Some("zzz_proj"),
        &["model.zzz_proj.root_model", "model.aaa_pkg.a"],
    );
    let (scoped, _) = apply_package_scope(dag, dbtl::PackageScope::Root, false);
    assert_eq!(scoped.len(), 1, "only the root project's model survives");
    assert!(scoped.contains("model.zzz_proj.root_model"));
}

#[test]
fn all_scope_keeps_every_package_node() {
    // `--include-packages`: the manifest's full contents stay — installed
    // package nodes alongside the root project's own.
    let dag = dag_with(
        Some("zzz_proj"),
        &["model.zzz_proj.root_model", "model.aaa_pkg.a"],
    );
    let (scoped, _) = apply_package_scope(dag, dbtl::PackageScope::All, false);
    assert_eq!(scoped.len(), 2, "package nodes stay in scope");
    assert!(scoped.contains("model.aaa_pkg.a"));
}

/// The load warnings the fusion fixture produces (unsupported
/// resource types) already printed to stderr must ALSO land in the
/// App's notification log as unread WARNING rows — `Ctrl-n` replays them
/// after the TUI opens, and `!N` counts them.
#[test]
fn build_app_routes_startup_warnings_into_the_notification_log() {
    let fusion = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fusion_manifest.json"
    );
    let cli = Cli::parse_from(["dbtl", "--manifest", fusion]);
    let app = build_app(&cli).expect("build_app over the fusion fixture");
    assert!(
        app.unread_warnings() > 0,
        "startup load warnings count toward !N"
    );
    let view = app.notifications_snapshot();
    assert_eq!(
        view.rows.len(),
        app.unread_warnings(),
        "every recorded row is an unread warning here"
    );
    assert!(
        view.rows
            .iter()
            .all(|r| r.level == dbtl::NoticeLevel::Warning
                && r.stamp.len() == 8
                && r.stamp.contains(':')),
        "each row is a stamped warning: {:?}",
        view.rows
    );
}
