use super::*;

// ---------------------------------------------------------------------------
// Column-level lineage: the structure modal's column rows carry
// up/down ONE-HOP edge counts, h/l walks a column cursor, and `y l` yanks the
// selected column's downstream closure as the shared ColumnReport markdown.
// The sample fixture's `cll_demo` is the predictable end: it declares no
// columns (so the rows come from the resolver's SQL extraction) and every
// output has the same 4-edge one-hop upstream / 0 downstream.
// ---------------------------------------------------------------------------
#[test]
fn detail_open_snapshots_column_counts_and_inferred_names() {
    let mut a = sample_app();
    let dv = open_detail(&mut a, "model.sample.cll_demo").clone();
    assert_eq!(
        dv.col_names,
        vec!["n_orders", "order_id", "status", "status_upper"],
        "no declared columns → the resolver's sorted inferred names"
    );
    assert_eq!(dv.col_index, 0, "cursor homes to the first column");
    for c in &dv.col_names {
        // One-hop counts (direct parents in / direct children out) — the
        // contract, NOT the transitive closure (which was 6 here).
        assert_eq!(dv.col_counts[c], (4, 0), "{c} one-hop counts");
    }
    assert!(!dv.stale, "the frozen fixture's compiled SQL is fresh");
}

#[test]
fn detail_column_move_clamps_at_both_ends() {
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.cll_demo");
    for _ in 0..10 {
        apply_action(&mut a, Action::DetailColumnMove(Direction::Down));
    }
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.col_index, 3, "clamped at the last column"),
        m => panic!("got {m:?}"),
    }
    for _ in 0..10 {
        apply_action(&mut a, Action::DetailColumnMove(Direction::Up));
    }
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.col_index, 0, "clamped at the first column"),
        m => panic!("got {m:?}"),
    }
}

#[test]
fn detail_column_move_noops_when_the_node_has_no_columns() {
    let mut a = sample_app();
    // The seed records no columns in the manifest and emits no SQL — the one
    // genuinely columnless node in the fixture. (It's upstream of fct_orders,
    // so a lineage click homes the cursor there.)
    a.select_by_unique_id("model.sample.fct_orders");
    a.ui_state.set_focus(Focus::RightPane);
    node_click(&mut a, "seed.sample.country_codes");
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert!(dv.col_names.is_empty(), "source shows no column rows");
            assert_eq!(dv.col_index, usize::MAX, "no cursor without columns");
        }
        m => panic!("expected Detail, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailColumnMove(Direction::Down));
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.col_index, usize::MAX, "still no cursor"),
        m => panic!("got {m:?}"),
    }
}

#[test]
fn yank_column_lineage_copies_the_selected_columns_downstream_closure() {
    let mut a = sample_app();
    let dv = open_detail(&mut a, "model.sample.stg_orders").clone();
    assert_eq!(dv.col_index, 0);
    assert_eq!(dv.col_names[0], "id", "the declared order starts at id");
    apply_action(&mut a, Action::YankPrefix);
    assert!(matches!(a.mode, Mode::Yank { .. }));
    assert_eq!(
        a.pending_col
            .as_ref()
            .map(|c| (c.node.as_str(), c.column.as_str())),
        Some(("model.sample.stg_orders", "id")),
        "the column under the cursor is stashed for `yl`"
    );
    let out = apply_action(&mut a, Action::YankColumnLineage);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(
        text.contains("# Column lineage: `stg_orders.id` (downstream)"),
        "same ColumnReport markdown as the CLI, got: {text:?}"
    );
    // v1: the parser's FOJ fans reach all four cll_demo outputs and every
    // cll_syntax output — 14 rows.
    assert!(text.contains("## Downstream columns (14)"));
    assert!(text.contains("`fct_orders.id`"));
    assert_eq!(a.take_notice().as_deref(), Some("Copied column lineage"));
    // One-shot close RESTORES the structure modal the which-key stacked over
    // (the nested-overlay rule — `back` is the Detail mode).
    assert!(matches!(a.mode, Mode::Detail(_)), "returns to the modal");
}

#[test]
fn yank_column_lineage_follows_the_column_cursor() {
    // Move the cursor first: `y l` must yank the column the cursor ENDED on,
    // not the first row.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.cll_demo");
    apply_action(&mut a, Action::DetailColumnMove(Direction::Down));
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankColumnLineage);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(
        text.contains("# Column lineage: `cll_demo.order_id` (downstream)"),
        "col_index 1 = order_id, got: {text:?}"
    );
    assert!(
        text.contains("## Downstream columns (0)"),
        "terminal column"
    );
}

#[test]
fn yank_column_lineage_without_a_column_explains_instead_of_yanking() {
    let mut a = sample_app();
    // `y` from Selection mode: no column under a cursor → nothing to yank.
    apply_action(&mut a, Action::YankPrefix);
    assert!(matches!(a.mode, Mode::Yank { .. }));
    let out = apply_action(&mut a, Action::YankColumnLineage);
    assert!(out.effects.is_empty(), "no clipboard write");
    assert_eq!(
        a.take_notice().as_deref(),
        Some("no column selected — Enter, h/l pick a column, then y l")
    );
}

#[test]
fn yank_menu_over_the_modal_targets_the_modal_node_not_the_selection() {
    // The hostile-review finding: open the modal on node A while the list
    // selection sits on node B — EVERY `y<key>` yank must act on A (the
    // modal's node), never silently on B.
    let mut a = sample_app();
    let dv = open_detail(&mut a, "model.sample.stg_orders").clone();
    assert_eq!(dv.model_id, "model.sample.stg_orders");
    // Move the LIST selection elsewhere (the modal stays open on stg_orders).
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::YankPrefix);
    assert!(matches!(a.mode, Mode::Yank { .. }));
    assert_eq!(
        a.pending_node.as_deref(),
        Some("model.sample.stg_orders"),
        "the modal's node is stashed as the yank target"
    );
    let out = apply_action(&mut a, Action::YankId);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(
        text, "model.sample.stg_orders",
        "yy copies the modal's uid, not the list selection's fct_orders"
    );
    // One-shot close restores the stacked modal (nested-overlay rule), and
    // the stash dies with the which-key either way.
    assert!(matches!(a.mode, Mode::Detail(_)), "returns to the modal");
    assert!(a.pending_node.is_none(), "the stash dies with the menu");
}

#[test]
fn yank_name_and_impact_over_the_modal_also_follow_the_modal_node() {
    // (The which-key is one-shot, so each yank gets its own modal-open pass.)
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankName);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "stg_orders", "yY copies the modal node's name");

    // The impact report is the second selection-based surface — same flow:
    // modal on stg_orders, selection on fct_orders, `yi` reports the MODAL
    // node's blast radius.
    open_detail(&mut a, "model.sample.stg_orders");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankImpact);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(
        text.starts_with("# Impact: stg_orders\n"),
        "yi reports the modal node, got: {text:?}"
    );
    assert!(text.contains("`model.sample.stg_orders`"));
}

#[test]
fn yank_mermaid_over_the_modal_draws_the_modal_nodes_lineage() {
    // Discriminating case: `country_codes` is upstream of fct_orders ONLY —
    // it appears in fct_orders' rooted lineage but nowhere in stg_orders'.
    // If the yank silently used the list selection it could never contain it.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.fct_orders");
    a.select_by_unique_id("model.sample.stg_orders");
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankMermaid);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert!(
        text.contains("country_codes"),
        "the diagram is the MODAL node's (fct_orders') lineage — country_codes \
         is upstream of fct_orders but absent from stg_orders' lineage: {text:?}"
    );
}

#[test]
fn yank_without_the_modal_keeps_the_selection_semantics() {
    // Guard the fallback: `y` opened from Selection mode stashes no modal
    // node, so the yank target stays the list selection.
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::YankPrefix);
    assert!(matches!(a.mode, Mode::Yank { .. }));
    assert!(a.pending_node.is_none(), "no modal → nothing stashed");
    let out = apply_action(&mut a, Action::YankId);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "model.sample.fct_orders");
}

#[test]
fn a_stale_modal_stash_cannot_leak_into_a_later_selection_yank() {
    // Open the modal on A, press `y`, cancel with an unbound key (closes the
    // menu), move the selection, press `y` again from Selection — the second
    // yank must target the SELECTION, not the cancelled modal's node.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    apply_action(&mut a, Action::YankPrefix);
    apply_action(&mut a, Action::DetailClose); // cancel: back to the stacked modal
    assert!(
        matches!(a.mode, Mode::Detail(_)),
        "Esc on the which-key restores the modal it stacked over"
    );
    apply_action(&mut a, Action::DetailClose); // then close the modal itself
    assert_eq!(a.mode, Mode::Selection);
    assert!(a.pending_node.is_none(), "cancel drops the stash");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankId);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "model.sample.fct_orders", "no stale modal target");
}

#[test]
fn help_over_the_yank_menu_keeps_the_modal_stash_alive() {
    // The review sequence: Detail → y → ? → Esc → yy. The `?` help LAYERS
    // over the which-key menu (back = Yank) — the stashed modal target must
    // survive it, or the yank silently hits the list selection instead of
    // the modal's node (a quiet wrong-target, the worst kind).
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::YankPrefix);
    assert_eq!(
        a.pending_node.as_deref(),
        Some("model.sample.stg_orders"),
        "the modal's node is stashed"
    );
    apply_action(&mut a, Action::HelpToggle);
    match &a.mode {
        Mode::Help(h) => assert!(
            matches!(h.back.as_ref(), Mode::Yank { .. }),
            "help layers over the menu"
        ),
        _ => panic!("expected Help mode"),
    }
    assert_eq!(
        a.pending_node.as_deref(),
        Some("model.sample.stg_orders"),
        "the stash survives the help layer"
    );
    // Esc peels back to the menu — which still knows its target.
    apply_action(&mut a, Action::HelpToggle);
    assert!(matches!(a.mode, Mode::Yank { .. }));
    let out = apply_action(&mut a, Action::YankId);
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(
        text, "model.sample.stg_orders",
        "yy still yanks the modal's node, not the list selection's fct_orders"
    );
    assert!(a.pending_node.is_none(), "the yank itself closes the stash");
}

#[test]
fn set_group_by_meta_treats_blank_keys_as_none() {
    // `group_by_meta: " "` in config.yml must NOT bucket every model under a
    // literal `(no   )` header — blank/whitespace keys normalize to `None`
    // (the default layer grouping).
    let mut a = app();
    assert_eq!(a.group_by_meta, None, "fresh app: layer grouping");

    a.set_group_by_meta(Some("owner".to_string()));
    assert_eq!(a.group_by_meta.as_deref(), Some("owner"));

    a.set_group_by_meta(Some("   ".to_string()));
    assert_eq!(a.group_by_meta, None, "whitespace-only key → None");
    assert!(
        a.model_list
            .groups
            .iter()
            .all(|g| !g.layer.contains("(no ")),
        "no `(no <key>)` bucket when grouping is off: {:?}",
        a.model_list
            .groups
            .iter()
            .map(|g| &g.layer)
            .collect::<Vec<_>>()
    );

    a.set_group_by_meta(Some("owner".to_string()));
    a.set_group_by_meta(Some(String::new()));
    assert_eq!(a.group_by_meta, None, "empty key → None");
}

// ---- column-level lineage focus ---------------------------------
//
// The CLL-demo fixture (`SAMPLE` = `tests/fixtures/sample_manifest.json`):
// `raw.orders → stg_orders → fct_orders → cll_demo`, exercising all four edge
// kinds (rename / passthrough / transform / filter-or-join) — the same graph
// `tests/cll.rs` pins the resolver against. (`FCT` already names the
// jaffle_finance fixture's model — these tests use `FCT_S` for sample's.)

const FCT_S: &str = "model.sample.fct_orders";
const DEMO: &str = "model.sample.cll_demo";
const RAW_ORDERS: &str = "source.sample.raw.orders";
const CODES: &str = "seed.sample.country_codes";

#[test]
fn column_focus_enter_filters_the_subgraph_to_the_trace() {
    // Enter from the structure modal's column list commits the cursored column
    // to the trace view: the subgraph shrinks to the column's related models,
    // the status chip appears, and the modal closes.
    let mut a = sample_app();
    a.select_by_unique_id(DEMO);
    apply_action(&mut a, Action::DetailOpen);
    let dv = match &a.mode {
        Mode::Detail(dv) => dv,
        m => panic!("expected Detail, got {m:?}"),
    };
    // cll_demo declares no columns; the resolver's inferred list fills
    // `col_names` — order_id first (BTreeMap key order).
    let i = dv
        .col_names
        .iter()
        .position(|c| c == "order_id")
        .expect("order_id is an inferred column");
    a.mode = Mode::Detail(DetailView {
        col_index: i,
        ..match &a.mode {
            Mode::Detail(dv) => dv.clone(),
            m => panic!("expected Detail, got {m:?}"),
        }
    });

    apply_action(&mut a, Action::ColumnFocusEnter);
    assert!(matches!(a.mode, Mode::Selection), "the modal closes");
    let focus = a
        .tab()
        .column_focus
        .as_ref()
        .expect("column focus is active");
    assert_eq!(focus.col.node, DEMO);
    assert_eq!(focus.col.column, "order_id");
    // The trace's related models = the three models + the raw source + the
    // country_codes seed the v1 parser reaches through fct_orders' join
    // (v0's name-match couldn't see it). `related` is a BTreeSet —
    // uid-sorted iteration order.
    assert_eq!(
        focus.related.iter().map(String::as_str).collect::<Vec<_>>(),
        vec![DEMO, FCT_S, STG, CODES, RAW_ORDERS],
    );
    let sg = a.lineage_subgraph();
    assert_eq!(sg.selected, DEMO, "the traced model is the graph's home");
    assert_eq!(
        sg.nodes.len(),
        5,
        "the filtered subgraph keeps only related nodes"
    );
    // The status chip reads `column:model.col ↑N ↓M` with ONE-HOP counts —
    // the same direct-neighbour rule as the structure modal's per-column
    // counts (order_id: 4 direct upstream, 0 downstream; NOT the 6-column
    // transitive closure the columns modal's section header quotes).
    let status = a.column_status().expect("the chip shows while focused");
    assert_eq!(status, "column:cll_demo.order_id ↑4 ↓0");
    assert_eq!(
        a.column_focus_label().as_deref(),
        Some(" [col:order_id]"),
        "the title suffix names the traced column"
    );
    // h/l stay free to step the column: the focus pins the pane.
    assert_eq!(a.ui_state.focus(), Focus::RightPane);
}

#[test]
fn column_focus_layout_draws_related_columns_inside_the_boxes() {
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(DEMO, "order_id"));
    let lay = a.styled_lineage_layout().expect("a layout exists");
    assert!(
        !lay.col_cells.is_empty(),
        "column rows recorded their interior cells"
    );
    let text = lay.grid.to_text();
    // The traced column carries the `>` mark inside cll_demo's box; each
    // parent box lists the columns that feed it (~ = rename upstream).
    assert!(text.contains("> order_id"), "traced row drawn: {text}");
    assert!(
        text.contains("~ id"),
        "rename-marked upstream column: {text}"
    );
    // Unrelated model columns never appear — only the trace's set.
    let rect = &lay.rects[DEMO];
    assert!(rect.height > 3, "the traced box grew for its column rows");
}

#[test]
fn column_focus_step_walks_the_column_along_its_lineage() {
    // h = upstream hop, l = downstream hop; the deterministic pick prefers a
    // same-named column (the passthrough continuation). Under the v1 parser
    // `cll_demo.status` comes from stg_orders ONLY — v0's name-match also
    // bound fct_orders.status (documented over-connection), so the chain no
    // longer detours through fct.
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(DEMO, "status"));
    apply_action(&mut a, Action::MoveLeft);
    let f = a.tab().column_focus.as_ref().unwrap();
    assert_eq!(f.col.node, STG, "h steps upstream to the real source");
    assert_eq!(f.col.column, "status");
    apply_action(&mut a, Action::MoveLeft);
    let f = a.tab().column_focus.as_ref().unwrap();
    assert_eq!(f.col.node, RAW_ORDERS, "the chain reaches the raw source");
    apply_action(&mut a, Action::MoveLeft);
    let f = a.tab().column_focus.as_ref().unwrap();
    assert_eq!(f.col.node, RAW_ORDERS, "a source column is a dead end");
    // Back downstream — deterministic (smallest uid column-ref).
    apply_action(&mut a, Action::MoveRight);
    let f = a.tab().column_focus.as_ref().unwrap();
    assert_eq!(f.col.node, STG, "l steps back downstream");
}

#[test]
fn column_focus_exit_restores_the_rooted_view() {
    let mut a = sample_app();
    a.select_by_unique_id(DEMO);
    a.column_focus_enter(ColumnRef::new(DEMO, "order_id"));
    assert!(a.tab().column_focus.is_some());
    apply_action(&mut a, Action::ColumnFocusExit);
    assert!(a.tab().column_focus.is_none(), "Esc exits the trace");
    assert!(a.column_status().is_none());
    let sg = a.lineage_subgraph();
    assert_eq!(sg.selected, DEMO, "the rooted subgraph returns");
    // The rooted default (upstream + downstream) of a terminal model differs
    // from the trace's filtered set — the focus graph is gone.
    let lay = a.styled_lineage_layout().expect("a layout exists");
    assert!(
        lay.col_cells.is_empty(),
        "no column rows without an active focus"
    );
}

#[test]
fn columns_modal_lists_the_tree_and_enter_jumps_to_the_model() {
    let mut a = sample_app();
    a.select_by_unique_id(STG);
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    apply_action(&mut a, Action::ColumnsOpen);
    let (rows_before, cursor) = match &a.mode {
        Mode::Columns(cv) => {
            // Headers bracket the two sections; the cursor sits on the
            // traced-column row.
            assert!(cv.rows.iter().any(|r| r.label == "this column:"));
            assert!(
                cv.rows.iter().any(|r| r.label.starts_with("upstream (")),
                "upstream section header"
            );
            assert!(
                cv.rows.iter().any(|r| r.label.starts_with("downstream (")),
                "downstream section header"
            );
            assert_eq!(cv.rows[cv.cursor].uid.as_deref(), Some(STG));
            assert!(cv.rows[cv.cursor].traced, "the cursor row IS the trace");
            (cv.rows.len(), cv.cursor)
        }
        m => panic!("expected Columns, got {m:?}"),
    };
    assert!(rows_before >= 4, "headers + traced + some entries");

    // j walks the ROW cursor to the first downstream entry (skipping the
    // section header), and Enter re-roots to that row's model.
    apply_action(&mut a, Action::ColumnRowMove(Direction::Down));
    let jumped_uid = match &a.mode {
        Mode::Columns(cv) => {
            assert!(
                cv.cursor > cursor,
                "j advanced the cursor past the header: {cursor} -> {}",
                cv.cursor
            );
            let row = &cv.rows[cv.cursor];
            let uid = row.uid.clone().expect("entry rows carry a uid");
            assert_ne!(uid, STG, "the cursor moved off the traced row");
            uid
        }
        m => panic!("expected Columns, got {m:?}"),
    };
    let _ = rows_before;
    apply_action(&mut a, Action::ColumnJump);
    assert_eq!(a.mode, Mode::Selection, "the modal closes on the jump");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(jumped_uid.as_str()),
        "Enter re-roots to the row's model"
    );
    assert_eq!(a.tab().back.len(), 1, "the jump records history");
    assert!(
        a.tab().column_focus.is_none(),
        "a re-root leaves the column trace"
    );
}

#[test]
fn columns_modal_jump_to_a_non_model_notices_instead() {
    // A raw-source column row is listed for context but can't be re-rooted to.
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    apply_action(&mut a, Action::ColumnsOpen);
    // Walk the cursor up into the upstream section onto the source row.
    apply_action(&mut a, Action::ColumnRowMove(Direction::Up));
    let uid = match &a.mode {
        Mode::Columns(cv) => cv.rows[cv.cursor].uid.clone(),
        m => panic!("expected Columns, got {m:?}"),
    };
    assert_eq!(uid.as_deref(), Some(RAW_ORDERS));
    apply_action(&mut a, Action::ColumnJump);
    assert!(
        matches!(a.mode, Mode::Columns(_)),
        "the modal stays open on a non-model row"
    );
    assert!(a.take_notice().is_some(), "a notice explains the no-op");
    assert!(
        a.tab().column_focus.is_some(),
        "focus survives a failed jump"
    );
}

#[test]
fn columns_open_without_a_focus_or_column_cursor_notices() {
    // `K` on the bare selection with no traced column can't open the modal.
    let mut a = sample_app();
    apply_action(&mut a, Action::ColumnsOpen);
    assert!(matches!(a.mode, Mode::Selection));
    assert!(a.take_notice().is_some());
}

#[test]
fn column_focus_exit_and_reload_clear_the_trace() {
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    assert!(a.reload().is_ok());
    assert!(
        a.tab().column_focus.is_none(),
        "a reload drops the trace (the new Dag may not carry the column)"
    );
}

#[test]
fn column_impact_lens_skips_the_cycle_without_a_focus_and_tints_downstream() {
    // Without a focus the ColumnImpact slot is dead — the `t` cycle jumps over
    // it (the reducer's dead-slot loop), never landing on a no-op lens.
    let mut a = sample_app();
    a.ui_state.set_lens(LineageLens::RunTime);
    apply_action(&mut a, Action::CycleLens);
    assert_eq!(
        a.ui_state.lens(),
        LineageLens::PrimaryKey,
        "ColumnImpact is skipped unfocused; PrimaryKey (data-free) is not"
    );
    // Focused, the cycle reaches it and it tints exactly the traced column's
    // downstream nodes.
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    a.ui_state.set_lens(LineageLens::RunTime);
    apply_action(&mut a, Action::CycleLens);
    assert_eq!(a.ui_state.lens(), LineageLens::ColumnImpact);
    let styles = a.lineage_styles(&a.styled_lineage_layout().unwrap());
    let fct_attr = styles.get(FCT_S).expect("fct is on the trace");
    assert_eq!(
        fct_attr.lens,
        LensTint::ColumnImpact,
        "a downstream node glows under the lens"
    );
    let raw_attr = styles.get(RAW_ORDERS).expect("the source is on the trace");
    assert_eq!(
        raw_attr.lens,
        LensTint::None,
        "an upstream node stays untinted"
    );
}

#[test]
fn column_focus_survives_an_overview_round_trip() {
    // The overview temporarily suppresses the focus (its filtered graph would
    // contradict the whole-graph view); the state survives and returns on exit.
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(DEMO, "order_id"));
    apply_action(&mut a, Action::ToggleOverview);
    assert!(a.ui_state.overview());
    assert!(a.column_status().is_none(), "the focus is suppressed");
    assert!(
        a.tab().column_focus.is_some(),
        "but the state itself survives"
    );
    apply_action(&mut a, Action::ToggleOverview);
    assert!(a.column_status().is_some(), "the focus returns on exit");
}

#[test]
fn detail_enter_on_a_columnless_modal_keeps_the_classic_close() {
    // A node whose modal has no column rows keeps Enter == close (the Issue
    // 07 contract — the focus arm only fires with a live column cursor). A
    // seed is guaranteed columnless (no SQL for the resolver, no declared
    // columns); navigate the lineage cursor onto it — fct_orders' upstream
    // subgraph contains the seed — so DetailOpen builds its modal.
    let mut a = sample_app();
    a.select_by_unique_id(FCT_S);
    a.ui_state.set_focus(Focus::RightPane);
    a.tab_mut().lineage_cursor = Some("seed.sample.country_codes".to_string());
    apply_action(&mut a, Action::DetailOpen);
    let dv = match &a.mode {
        Mode::Detail(dv) => dv,
        m => panic!("expected Detail, got {m:?}"),
    };
    assert!(
        dv.col_names.is_empty(),
        "a seed's modal has no column cursor: {:?}",
        dv.col_names
    );
    apply_action(&mut a, Action::ColumnFocusEnter);
    assert_eq!(a.mode, Mode::Selection, "Enter still closes the modal");
    assert!(a.tab().column_focus.is_none(), "no focus without a column");
}

#[test]
fn column_focus_survives_clicks_on_the_trace_root_and_non_models() {
    // Clicking the traced model's own box (or a non-model node in the
    // filtered graph) is interacting WITH the trace, not leaving it — only a
    // re-root to a DIFFERENT model ends the focus (the columns modal's Enter
    // applies the same "failed jump keeps focus" rule).
    let mut a = sample_app();
    a.select_by_unique_id(DEMO);
    a.column_focus_enter(ColumnRef::new(DEMO, "order_id"));

    node_click(&mut a, DEMO);
    assert!(
        a.tab().column_focus.is_some(),
        "clicking the trace's root keeps the focus"
    );
    node_click(&mut a, RAW_ORDERS);
    assert!(
        a.tab().column_focus.is_some(),
        "a non-model click moves the cursor, not the root"
    );
    assert_eq!(
        a.tab().lineage_cursor.as_deref(),
        Some(RAW_ORDERS),
        "the cursor landed on the clicked source"
    );

    node_commit(&mut a, FCT_S);
    assert!(
        a.tab().column_focus.is_none(),
        "re-rooting to another model leaves the trace"
    );
    assert_eq!(a.selected_unique_id().as_deref(), Some(FCT_S));
}

#[test]
fn column_focus_survives_when_the_trace_root_is_not_the_selection() {
    // The trace's root is `col.node`, not the list selection: trace a column
    // of STG while DEMO is selected, then jump to STG — the selection moves
    // (a real re-root), yet the target IS the trace's home, so the trace
    // stays.
    let mut a = sample_app();
    a.select_by_unique_id(DEMO);
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    a.jump_to(STG);
    assert!(
        a.tab().column_focus.is_some(),
        "landing on the trace's own model"
    );
    assert_eq!(a.selected_unique_id().as_deref(), Some(STG));
    // From there, jumping elsewhere still leaves the trace.
    a.jump_to(DEMO);
    assert!(a.tab().column_focus.is_none());
}

#[test]
fn column_focus_ends_on_history_and_cycle_re_roots() {
    // History nav is a re-root: `b` back to a different model ends the trace,
    // while a hop landing ON the traced model keeps it (same contract as
    // jump_to). The bookmark/gap cycles follow the same rule.
    let mut a = sample_app();
    a.select_by_unique_id(FCT_S);
    a.jump_to(STG); // back = [FCT_S]
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    apply_action(&mut a, Action::HistoryBack);
    assert_eq!(a.selected_unique_id().as_deref(), Some(FCT_S));
    assert!(
        a.tab().column_focus.is_none(),
        "history back re-rooted off the trace"
    );

    // Re-enter the trace on STG, then `f` forward — landing back on the
    // traced model keeps the focus (trace_root == target).
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    apply_action(&mut a, Action::HistoryForward);
    assert_eq!(a.selected_unique_id().as_deref(), Some(STG));
    assert!(
        a.tab().column_focus.is_some(),
        "history forward lands on the trace's own model"
    );

    // Bookmark cycle to a DIFFERENT model is likewise a re-root.
    a.bookmarks.insert(DEMO.to_string());
    a.bookmarks.insert(FCT_S.to_string());
    apply_action(&mut a, Action::BookmarkCycle);
    assert_ne!(
        a.selected_unique_id().as_deref(),
        Some(STG),
        "the cycle moved to another bookmark"
    );
    assert!(
        a.tab().column_focus.is_none(),
        "the cycle re-root ends the trace"
    );
}

#[test]
fn column_focus_gates_view_filters_with_a_notice() {
    // u/d/[/]/0 would silently mutate the rooted view — which the trace then
    // reveals on Esc. Like the overview gate, they notice instead; the `v`
    // density stays live (Compact legitimately hides the column rows).
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(DEMO, "order_id"));
    let before = a.tab().lineage_view.clone();
    for action in [
        Action::ToggleUpstream,
        Action::ToggleDownstream,
        Action::DepthDecrease,
        Action::DepthIncrease,
        Action::ResetView,
    ] {
        apply_action(&mut a, action);
        assert_eq!(
            a.tab().lineage_view,
            before,
            "{action:?} must not mutate the view under a trace"
        );
        assert_eq!(
            a.take_notice().as_deref(),
            Some("view filters are off while tracing a column (Esc to exit)"),
            "{action:?} explains itself"
        );
    }
    apply_action(&mut a, Action::ToggleDensity);
    assert_ne!(
        a.ui_state.density(),
        crate::Density::Comfortable,
        "v still toggles density under the trace"
    );
}

#[test]
fn column_impact_lens_skips_under_an_overview_suppressed_focus() {
    // The overview suppresses the focus's DISPLAY but keeps its state — the
    // dead-slot test must read the EFFECTIVE focus, or `t` lands on a
    // tint-less ColumnImpact while the whole graph is up.
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    apply_action(&mut a, Action::ToggleOverview);
    assert!(a.ui_state.overview());
    a.ui_state.set_lens(LineageLens::RunTime);
    apply_action(&mut a, Action::CycleLens);
    assert_eq!(
        a.ui_state.lens(),
        LineageLens::PrimaryKey,
        "ColumnImpact is skipped while the overview suppresses the focus; \
         PrimaryKey (data-free) is not"
    );
}
