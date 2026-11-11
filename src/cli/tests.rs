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

// ---- the exit-code + ignored-flag contract -------------------------------

use super::exit::ExitKind;
use super::flags::{is_ignored_flag_exempt, GLOBAL_FLAG_COMMANDS, TUI_ONLY_FLAGS};
use clap::{ArgAction, CommandFactory};
use std::collections::BTreeSet;

/// The pure mapping `main` applies — every failure class has exactly one
/// exit code, and an unclassified error is `3` (never confused with a clean
/// gate trip or a usage mistake).
#[test]
fn exit_code_maps_each_kind() {
    assert_eq!(exit_code(Some(ExitKind::Gate)), 1);
    assert_eq!(exit_code(Some(ExitKind::Usage)), 2);
    assert_eq!(exit_code(Some(ExitKind::Input)), 3);
    assert_eq!(exit_code(None), 3, "unclassified → input");
}

/// Every `global = true` clap arg appears in the accepted-flags table —
/// a new global flag must name its consuming subcommands (an empty list is
/// a valid "TUI only" answer), or the ignored-flag check can't vet it.
#[test]
fn flag_table_covers_every_global_arg() {
    let cmd = Cli::command();
    let globals: BTreeSet<String> = cmd
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(|a| a.get_id().to_string())
        .collect();
    let table: BTreeSet<String> = GLOBAL_FLAG_COMMANDS
        .iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(globals, table);
}

/// The table's subcommand names all exist — a typo would silently let the
/// flag through under a command that never runs.
#[test]
fn flag_table_names_real_subcommands() {
    let cmd = Cli::command();
    let names: BTreeSet<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for (flag, subs) in GLOBAL_FLAG_COMMANDS {
        for s in *subs {
            assert!(
                names.contains(*s),
                "table names unknown subcommand '{s}' for '{flag}'"
            );
        }
    }
}

/// `TUI_ONLY_FLAGS` IS the top-level non-global flag set, minus built-ins
/// (help/version) and the exempt deprecated `--project-only` — the clap
/// definitions and the rejection list can't drift apart.
#[test]
fn tui_only_flags_match_the_top_level_flag_set() {
    let cmd = Cli::command();
    let builtin = |a: &clap::Arg| {
        matches!(
            a.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
    };
    let expected: BTreeSet<String> = cmd
        .get_arguments()
        .filter(|a| !a.is_global_set() && !builtin(a))
        .map(|a| a.get_id().to_string())
        .filter(|id| !is_ignored_flag_exempt(id))
        .collect();
    let listed: BTreeSet<String> = TUI_ONLY_FLAGS.iter().map(|s| s.to_string()).collect();
    assert_eq!(expected, listed);
}

/// `check_flag_usage` reads `ArgMatches::value_source`: explicitly typed
/// TUI-only/global-but-unsupported flags are usage errors carrying both
/// names; accepted and absent flags pass through.
#[test]
fn flag_usage_rejects_unsupported_flags_by_value_source() {
    for args in [
        ["dbtl", "--watch", "check"].as_slice(),
        // A TUI-only flag AFTER the subcommand never reaches the table —
        // clap's own unknown-arg error exits 2 first. Kept in the list so
        // both rejection paths are pinned.
        ["dbtl", "check", "--watch"].as_slice(),
        ["dbtl", "--theme", "default", "check"].as_slice(),
        ["dbtl", "--run-results", "x.json", "check"].as_slice(),
        ["dbtl", "check", "--run-results", "x.json"].as_slice(),
        ["dbtl", "--diff", "x", "docs", "--out", "o"].as_slice(),
        ["dbtl", "columns", "a.b", "--diff", "x"].as_slice(),
    ] {
        match Cli::command().try_get_matches_from(args) {
            Err(e) => assert_eq!(e.exit_code(), 2, "{args:?}: clap's own usage error"),
            Ok(matches) => {
                let Err(err) = check_flag_usage(&matches) else {
                    panic!("{args:?} must be rejected");
                };
                assert_eq!(err.kind(), Some(ExitKind::Usage), "{args:?}");
                let msg = err.to_string();
                assert!(msg.contains("check") || msg.contains("docs") || msg.contains("columns"));
            }
        }
    }

    for args in [
        ["dbtl", "check"].as_slice(),
        ["dbtl", "--manifest", "m.json", "check"].as_slice(),
        ["dbtl", "check", "--manifest", "m.json", "--diff", "b.json"].as_slice(),
        ["dbtl", "--catalog", "c.json", "columns", "a.b"].as_slice(),
        ["dbtl", "--sql-dialect", "generic", "columns", "a.b"].as_slice(),
        // lint + docs consume both aux inputs (the SQL/catalog rules).
        ["dbtl", "docs", "--out", "o", "--sql-dialect", "generic"].as_slice(),
        ["dbtl", "--catalog", "c.json", "docs", "--out", "o"].as_slice(),
        [
            "dbtl",
            "lint",
            "--manifest",
            "m.json",
            "--catalog",
            "c.json",
        ]
        .as_slice(),
        ["dbtl", "lint", "--sql-dialect", "generic"].as_slice(),
        // The deprecated --project-only is exempt — its own warning carries
        // the "does nothing" message.
        ["dbtl", "--project-only", "check"].as_slice(),
    ] {
        let matches = Cli::command().get_matches_from(args);
        check_flag_usage(&matches).unwrap_or_else(|e| panic!("{args:?} must pass: {e}"));
    }
}
