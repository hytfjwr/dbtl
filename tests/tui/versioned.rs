//! The versioned manifest through the TUI surface: the left-pane list rows
//! and the lineage boxes must distinguish `fct_orders`' three versions —
//! three identical `fct_orders` labels were the ambiguity `list_name`
//! exists to fix.

use super::*;

/// The versioned fixture as a Dag (`v: 1`/`v: 2` numeric, `v: beta` string).
fn versioned_dag() -> Dag {
    load_dag(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/versioned_manifest.json"
    ))
    .expect("versioned fixture must load")
}

fn versioned_list() -> ModelList {
    build_model_list(&versioned_dag(), SortMode::Layer)
}

/// The initial frame for the versioned list — same draw path as the
/// fixture `render`.
fn render_versioned(width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = versioned_list();
    let state = UiState::new(list.len());
    let hints = list_hints();
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.hints = &hints;
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    terminal.backend().buffer().clone()
}

#[test]
fn left_pane_rows_distinguish_every_version() {
    let text = buffer_to_string(&render_versioned(100, 30));
    for want in ["fct_orders.v1", "fct_orders.v2", "fct_orders.vbeta"] {
        assert!(
            text.contains(want),
            "the left pane must show {want:?}, got:\n{text}"
        );
    }
}
