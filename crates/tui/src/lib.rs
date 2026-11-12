//! dbtl-tui — the interactive terminal application: keymap, reducer, and
//! ratatui rendering over [`dbtl_core`]'s `Dag`.
//!
//! Every domain type is re-exported at this crate's root (`pub use
//! dbtl_core::*`), so module code addresses data types as `crate::Dag` etc.
//! regardless of which crate they physically live in — the crate boundary is
//! enforced by the dependency graph (this crate may use `dbtl_core`; the
//! reverse is impossible), not by import paths.

pub use dbtl_core::*;

pub mod action;
pub mod app;
// Crate-internal: the lineage-pane sibling-collapse stage (Subgraph -> the
// Subgraph layout draws, plus the marker index). Not part of the public API —
// interaction goes through `App`.
mod collapse;
pub mod effect;
pub mod model_list;
pub mod persist;
pub mod ui;

pub use action::{
    dispatch, help_lines, Action, DiffView, Direction, HelpLine, HelpView, InspectorLintFinding,
    InspectorView, LintWorklist, Mode, ModeKind, NoticeLevel, NotificationEntry, NotificationsView,
    PaletteRow, SearchTarget, SqlView, StatsView,
};
pub use app::{
    apply_action, layer_violation_edges, App, AppStats, HomeData, HomeItem, HomeRow, HomeSearch,
    HomeSection, HomeTone, ListFilter, Outcome,
};
pub use effect::Effect;
pub use model_list::{
    build_filtered_model_list, build_model_list, build_model_list_grouped, match_indices,
    name_matches_query, DisplayRow, ModelGroup, ModelList, SortMode, LAYER_ORDER,
};
pub use persist::{Config, PersistedState};
pub use ui::{
    draw, handle_key, reduce_selection, Focus, InspectorPolicy, KeyOutcome, LineageLens, RenderCtx,
    ToastLine, UiState,
};
