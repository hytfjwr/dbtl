//! `App` tests, split by concern: the shared fixture loaders, synthetic-Dag
//! builders, and click/commit simulators live here; each submodule covers one
//! feature area and inherits the helpers below via `use super::*`.

pub use super::*;
pub use crate::action::{
    palette_candidates, Action, DetailView, DiffView, Direction, HelpView, Mode, ModeKind,
    NoticeLevel, NotificationEntry, PaletteRow, SearchTarget,
};
pub use crate::effect::Effect;
pub use crate::ui::{
    hit_target, lineage_content_rect, minimap_rect, pane_interior, pane_layout, HitGeometry,
    HitTarget, PaneConfig, Splitter, DEFAULT_INSPECTOR_WIDTH, DEFAULT_LIST_WIDTH, MIN_LIST_WIDTH,
};
pub use crate::{
    coverage_gap, load_dag, load_dag_from_source, reduce_selection, Dag, DisplayRow, Focus, Layout,
    LensTint, LineageLens, MaterializationClass, NodeInfo, SortMode, UiState,
};

pub use ratatui::crossterm::event::MouseButton;
pub use ratatui::layout::Rect;

pub use std::path::{Path, PathBuf};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/manifest.json"
);

fn app() -> App {
    let dag = load_dag(FIXTURE).expect("fixture loads");
    let mut a = App::new(dag, PathBuf::from(FIXTURE));
    // The fixture carries sibling groups of 11-24 — far over the collapse
    // default — and every expectation in this file was written pre-collapse.
    // Pin the threshold to 0 (disabled) so they assert the uncollapsed layout;
    // the sibling-collapse tests build their own synthetic dags instead.
    a.set_collapse_threshold(0);
    a
}

/// A zero-node `App` — every "empty X" surface's floor.
fn empty_app() -> App {
    use std::collections::HashMap;
    let dag = Dag::build(&crate::RawManifest {
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes: HashMap::new(),
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
    App::new(dag, PathBuf::from(FIXTURE))
}

/// Simulate a lineage node's DOUBLE-click (the commit): park the cursor on
/// `uid`, then run the Enter arm — a model re-roots, a `+M more` marker
/// toggles, anything else opens the structure modal — the same two calls the
/// event loop's `DoubleClick` arm makes.
fn node_commit(a: &mut App, uid: &str) {
    a.commit_lineage_node(uid);
    apply_action(a, Action::DetailOpen);
}

/// Simulate a lineage node's SINGLE left click: cursor-move only — the
/// production [`App::click_target`] path the event loop takes.
fn node_click(a: &mut App, uid: &str) {
    a.click_target(
        &HitTarget::LineageNode(uid.to_string()),
        MouseButton::Left,
        (10, 10),
    );
}

fn entry(level: NoticeLevel, text: &str) -> NotificationEntry {
    NotificationEntry {
        stamp: "00:00:00".into(),
        level,
        text: text.into(),
    }
}

// ---------------------------------------------------------------------------
// The CLL-demo fixture (`SAMPLE` = `tests/fixtures/sample_manifest.json`):
// `cll_demo` declares no columns (so the rows come from the resolver's SQL
// extraction) and every output has the same 4-edge one-hop upstream /
// 0 downstream.
// ---------------------------------------------------------------------------

const SAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/sample_manifest.json"
);

fn sample_app() -> App {
    let dag = load_dag(SAMPLE).expect("sample manifest loads");
    App::new(dag, PathBuf::from(SAMPLE))
}

fn open_detail<'a>(a: &'a mut App, uid: &str) -> &'a DetailView {
    a.select_by_unique_id(uid);
    apply_action(a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.model_id, uid);
            dv
        }
        m => panic!("expected Detail for {uid}, got {m:?}"),
    }
}

const FCT: &str = "model.jaffle_finance.fct_subscription_process";

const STG: &str = "model.sample.stg_orders";

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
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    };
    add("model.p.a", "a", "view");
    add("model.p.b", "b", "table");
    add("model.p.gone", "gone", "view");
    Dag::build(&RawManifest {
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
        parent_map: HashMap::from([("model.p.b".to_string(), vec!["model.p.a".to_string()])]),
        child_map: HashMap::from([("model.p.a".to_string(), vec!["model.p.b".to_string()])]),
        macros: HashMap::new(),
        docs: Default::default(),
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
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    };
    add("model.p.a", "a", "view");
    add("model.p.b", "b", "incremental"); // modified vs the base
    add("model.p.c", "c", "view"); // added vs the base
    Dag::build(&RawManifest {
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
        parent_map: HashMap::from([
            ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
            ("model.p.c".to_string(), vec!["model.p.b".to_string()]),
        ]),
        child_map: HashMap::from([
            ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
            ("model.p.b".to_string(), vec!["model.p.c".to_string()]),
        ]),
        macros: HashMap::new(),
        docs: Default::default(),
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
            ..Default::default()
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
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes: node_map,
        sources: HashMap::new(),
        exposures: exposure_map,
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
        docs: Default::default(),
    })
}

/// Write a run_results.json into a unique temp path and point `app` at it —
/// the same code path `--run-results` takes.
fn load_run_results(app: &mut App, results_json: &str) {
    let path = std::env::temp_dir().join(format!(
        "dbtl_rr_test_{}_{:?}.json",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, results_json).expect("write temp run_results");
    app.set_run_results(path.clone())
        .expect("parse run_results");
    let _ = std::fs::remove_file(path);
}

/// A three-model chain `stg_a -> fct_b -> mart_c` for the runtime tests.
fn run_dag() -> Dag {
    pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 1")),
            ("model.p.fct_b", pr_model("fct_b", "marts", "select 2")),
            ("model.p.mart_c", pr_model("mart_c", "marts", "select 3")),
        ],
        &[
            ("model.p.stg_a", "model.p.fct_b"),
            ("model.p.fct_b", "model.p.mart_c"),
        ],
        vec![],
    )
}

/// The MenuState for the currently-open menu (panics otherwise).
fn menu_state(a: &App) -> &crate::action::MenuState {
    match &a.mode {
        Mode::Menu(ms) => ms,
        m => panic!("expected Mode::Menu, got {m:?}"),
    }
}

/// The labels of a menu's Item rows, in display order.
fn menu_labels(items: &[crate::action::MenuEntry]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|e| match e {
            crate::action::MenuEntry::Item { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect()
}

/// The same bundle the event loop assembles per mouse event — the tests share
/// it so the headless hit-tests exercise the real wiring, not parallel math.
fn hit_geom<'a>(a: &'a App, lay: Option<&'a Layout>, area: Rect) -> HitGeometry<'a> {
    let panes = pane_layout(area, &a.ui_state.pane_config());
    HitGeometry {
        panes,
        home: None,
        // The strip resolved like the event loop does: the stored scroll
        // replayed (no ensure-active — the draw already ran it).
        tab_bar: Some(a.tab_bar_layout_for(panes.tab_bar, false)),
        list: a.active_list(),
        list_offset: a.ui_state.offset(),
        list_selected: a.ui_state.selected(),
        layout: lay,
        scroll_x: a.ui_state.lineage_scroll_x(),
        scroll_y: a.ui_state.lineage_scroll_y(),
        minimap: a.ui_state.minimap_visible(),
        overlay: None,
        // Headless hit-tests that need the span geometry build it
        // explicitly — the default stays the chrome-free surface.
        status: None,
        title: None,
    }
}

/// Three model uids off the fixture list (distinct from each other and from
/// the seeded first tab's root).
fn three_models(a: &App) -> (String, String, String) {
    let m = |i| a.active_list().model_at(i).unwrap().unique_id.clone();
    (m(0), m(1), m(2))
}

mod catalog;
mod collapse_badges;
mod columns;
mod diff_impact;
mod export;
mod folding;
mod history;
mod home;
mod impact;
mod inspector;
mod lenses;
mod lineage_cursor;
mod misc;
mod modals;
mod mouse;
mod navigation;
mod node_menu;
mod notifications;
mod overlay_hit;
mod overview;
mod palette_search_help;
mod run_results;
mod status_chrome;
mod tabs;
mod view_cycles;
