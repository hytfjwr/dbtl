use super::*;

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
    a.tab_mut().back = vec![
        survivor.into(),
        "model.jaffle_finance.ghost_does_not_exist".into(),
        survivor.into(),
    ];
    a.tab_mut().forward = vec!["model.jaffle_finance.ghost_ahead".into()];
    a.reload().expect("reload ok");
    assert_eq!(
        a.tab().back,
        vec![survivor.to_string()],
        "back keeps survivors only, adjacent duplicates collapsed"
    );
    assert!(
        a.tab().forward.is_empty(),
        "the vanished forward entry is dropped"
    );
    a.history_back();
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(survivor),
        "back lands on the most recent surviving entry, not a no-op"
    );
    assert_eq!(
        a.tab().forward,
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
    assert_eq!(a.tab().lineage_view.depth, Some(1));
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
        a.tab().lineage_view.depth,
        None,
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
    let project = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sample_project"
    );
    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sample_manifest.json"
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
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests")),
        "project root re-derived from the adopted manifest"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.stg_orders"),
        "selection restored by id after the swap"
    );
}

/// Write a minimal manifest.json (nodes + sources + the two adjacency maps
/// are the required keys) into `dir` and return its path — each uid becomes a
/// bare model node, enough for `load_dag` + the scope predicates.
fn write_min_manifest(dir: &Path, project_name: Option<&str>, uids: &[&str]) -> PathBuf {
    let nodes: serde_json::Map<String, serde_json::Value> = uids
        .iter()
        .map(|uid| {
            (
                uid.to_string(),
                serde_json::json!({
                    "name": uid.rsplit('.').next().unwrap_or(uid),
                    "resource_type": "model",
                }),
            )
        })
        .collect();
    let mut manifest = serde_json::json!({
        "nodes": nodes,
        "sources": {},
        "parent_map": {},
        "child_map": {},
    });
    if let Some(p) = project_name {
        manifest["metadata"] = serde_json::json!({ "project_name": p });
    }
    let path = dir.join("manifest.json");
    std::fs::write(&path, manifest.to_string()).expect("write manifest");
    path
}

#[test]
fn reload_and_adopt_return_the_scope_warnings() {
    // `reload`/`adopt_manifest` must surface the same warnings the CLI load
    // path prints — an emptied or ambiguously-rooted re-load is a load
    // outcome, not a silent re-scope.
    let tmp = tempfile::tempdir().unwrap();
    let path = write_min_manifest(tmp.path(), Some("zzz"), &["model.zzz.a", "model.aaa.b"]);
    let mut a = App::new(load_dag(&path).unwrap(), path.clone());
    assert!(
        a.reload().expect("clean reload").is_empty(),
        "an unremarkable reload warns about nothing"
    );

    // The recorded root vanishes between reloads: the scope now empties the
    // graph and the warning explains where every node went.
    write_min_manifest(tmp.path(), Some("zzz"), &["model.aaa.a", "model.aaa.b"]);
    let warnings = a.reload().expect("reload");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("no nodes belong to the root project 'zzz'")),
        "the emptied-graph warning reaches the caller, got {warnings:?}"
    );

    // No metadata + several packages: the root-package guess is flagged too.
    write_min_manifest(
        tmp.path(),
        None,
        &["model.aaa.a", "model.bbb.b", "model.bbb.c"],
    );
    let warnings = a.reload().expect("reload");
    assert!(
        warnings.iter().any(|w| w.contains("metadata.project_name")),
        "the ambiguous-root warning, got {warnings:?}"
    );

    // `adopt_manifest` forwards reload's warnings on success — and on a bad
    // path keeps the old source (the never-corrupt contract).
    let prev = a.model_list.len();
    assert!(a
        .adopt_manifest(PathBuf::from("/no/such/manifest.json"))
        .is_err());
    assert_eq!(a.model_list.len(), prev, "failed adopt leaves state intact");
    let path = write_min_manifest(tmp.path(), Some("zzz"), &["model.aaa.a", "model.aaa.b"]);
    let warnings = a.adopt_manifest(path).expect("adopt");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("no nodes belong to the root project 'zzz'")),
        "adopt forwards the emptied-graph warning, got {warnings:?}"
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
    assert!(matches!(a.mode, Mode::Help(_)), "? opens help");
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
